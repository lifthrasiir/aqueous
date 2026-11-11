//! A module built as a native library (crates/aqr-dll): the C interface it exports, which
//! applies its entry point under [`emu`](crate::emu) in the library itself, on the bytes the
//! caller gives, and the caller's side of it (crates/aqr-dll-shim).
//!
//! The library exports two functions:
//!
//! - `aqr_dll_info`, an [`InfoFn`]: what module it is.
//! - `aqr_dll_apply`, an [`ApplyFn`]: an application, its outputs written into the caller's
//!   buffers, each of the size declared; 0 if it succeeded, else 1 with a message of the failure
//!   in `error` (as much of it as fits, `*error_len` bytes).
//! - `aqr_dll_apply_part`, an [`ApplyPartFn`]: an application of a part module streamed, its
//!   inputs pulled through the caller's callbacks ([`PartIn`]) as the module requests them and
//!   its outputs pushed ([`PartOut`]) as it commits them, so that a big stream is never held
//!   whole; returning as `aqr_dll_apply` does.
//!
//! The library holds a copy of the module's code and of everything it uses (or links against the
//! shared libraries that do, crates/aqr-dll/layout.py): the standard library among them, its own
//! allocator, its own threads for the tasks.

use core::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::emu::{self, Drain, Entry, Feed, Kind};

/// The version of the interface, which [`Info::version`] tells: 2 has `aqr_dll_apply_part`, 3
/// how the module runs natively (`Info::run`, `aqr_dll_decode`).
pub const VERSION: u32 = 3;

/// Bytes given to a callback ([`PartIn::pull`]'s `put`).
pub type BytesFn = unsafe extern "C" fn(ctx: *mut c_void, ptr: *const u8, len: usize);

/// An input of a part application, as the module pulls it (the interpreter blob's too).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PartIn {
    pub ctx: *mut c_void,
    pub len: u64,
    /// Pulls the input up to offset `hi`, in pieces given to `put` with `put_ctx`; 0 if it did.
    pub pull: unsafe extern "C" fn(ctx: *mut c_void, hi: u64, put_ctx: *mut c_void, put: BytesFn) -> u32,
}

/// An output of a part application, taking the bytes as the module commits them.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PartOut {
    pub ctx: *mut c_void,
    /// Takes `len` bytes at `ptr`; 0 if it did.
    pub push: unsafe extern "C" fn(ctx: *mut c_void, ptr: *const u8, len: usize) -> u32,
}

/// `aqr_dll_apply_part`: the parameters, the resources, the inputs and outputs streamed, the
/// sizes of the outputs (`n_outputs` of them), and where the message of a failure goes.
pub type ApplyPartFn = unsafe extern "C" fn(
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const PartIn,
    n_inputs: usize,
    outputs: *const PartOut,
    n_outputs: usize,
    sizes: *const u64,
    error: SliceMut,
    error_len: *mut usize,
) -> u32;

/// Bytes the caller gives.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Slice {
    pub ptr: *const u8,
    pub len: usize,
}

/// Bytes the caller gives to be written.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SliceMut {
    pub ptr: *mut u8,
    pub len: usize,
}

impl Slice {
    pub fn of(b: &[u8]) -> Slice {
        Slice { ptr: b.as_ptr(), len: b.len() }
    }

    /// # Safety
    /// `ptr` points to `len` bytes alive for `'a` (or `len` is 0).
    pub unsafe fn get<'a>(self) -> &'a [u8] {
        match self.len {
            0 => &[],
            n => unsafe { std::slice::from_raw_parts(self.ptr, n) },
        }
    }
}

impl SliceMut {
    pub fn of(b: &mut [u8]) -> SliceMut {
        SliceMut { ptr: b.as_mut_ptr(), len: b.len() }
    }

    /// # Safety
    /// `ptr` points to `len` bytes alive for `'a` and not otherwise borrowed (or `len` is 0).
    pub unsafe fn get<'a>(self) -> &'a mut [u8] {
        match self.len {
            0 => &mut [],
            n => unsafe { std::slice::from_raw_parts_mut(self.ptr, n) },
        }
    }
}

/// What module a library is (`aqr_dll_info`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Info {
    /// [`VERSION`] of the library's interface.
    pub version: u32,
    /// 1 for a part module, 0 for a whole one.
    pub part: u32,
    /// The arguments of its entry point: the parameters, the resources, the inputs and the
    /// outputs, one each.
    pub arity: u32,
    /// The name of the module's binary in crates/aqr-modules (`lz4-recons`), UTF-8, static.
    pub name: Slice,
    /// Its `aqr_dll_apply_part`, for a part module (an address within the library as it is
    /// loaded, or merged: what a merged library is reached by, with no table of exports).
    pub apply_part: Option<ApplyPartFn>,
    /// How it runs natively (aqr-native's `Run`): [`RUN_WHOLE`], [`RUN_PART`], [`RUN_THREADED`]
    /// or [`RUN_STREAMED`].
    pub run: u32,
    /// 1 where natively it takes resources (those that do not are given none).
    pub resources: u32,
    /// Its native decoding of whole inputs (`aqr_dll_decode`), where it has one: what a host
    /// applies it whole by (on threads), rather than by the emulation of its entry point.
    pub decode: Option<DecodeFn>,
    /// For [`RUN_STREAMED`]: whether an application of inputs and an output of these lengths is
    /// to be streamed rather than applied whole.
    pub streamed: Option<StreamedFn>,
}

/// [`Info::run`]: applied whole, by its native decoding.
pub const RUN_WHOLE: u32 = 0;
/// [`Info::run`]: a part module, streamed, or applied whole by its native decoding where it has
/// one (else by the emulation), as its host judges.
pub const RUN_PART: u32 = 1;
/// [`Info::run`]: a part module in Wasm applied whole natively, by its native decoding on threads.
pub const RUN_THREADED: u32 = 2;
/// [`Info::run`]: as [`RUN_THREADED`], but streamed where [`Info::streamed`] says so.
pub const RUN_STREAMED: u32 = 3;

/// A module's native decoding of whole inputs (aqr-native's `Decode`): the parameters, the
/// resources, the inputs, the output's length and the threads it may use; the output.
pub type NativeDecode = fn(&[u8], &[&[u8]], &[&[u8]], u64, usize) -> Result<Vec<u8>, String>;

/// `aqr_dll_decode`: the parameters, the resources, the inputs, the output (of the length
/// declared, written), the threads it may use, and where the message of a failure goes; 0 if it
/// succeeded.
pub type DecodeFn = unsafe extern "C" fn(
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const Slice,
    n_inputs: usize,
    output: SliceMut,
    threads: usize,
    error: SliceMut,
    error_len: *mut usize,
) -> u32;

/// `aqr_dll_streamed`: 1 where an application of inputs of `lens` and an output of `len` bytes
/// is to be streamed.
pub type StreamedFn = unsafe extern "C" fn(lens: *const u64, n_lens: usize, len: u64) -> u32;

/// The body of `aqr_dll_decode` ([`DecodeFn`]) of native decoding `decode`.
///
/// # Safety
/// As [`DecodeFn`] is called.
#[allow(clippy::too_many_arguments)]
pub unsafe fn decode(
    decode: NativeDecode,
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const Slice,
    n_inputs: usize,
    output: SliceMut,
    threads: usize,
    error: SliceMut,
    error_len: *mut usize,
) -> u32 {
    let list = |p: *const Slice, n: usize| match n {
        0 => Vec::new(),
        n => unsafe { std::slice::from_raw_parts(p, n) }.iter().map(|s| unsafe { s.get() }).collect::<Vec<_>>(),
    };
    let (params, resources, inputs) = (unsafe { params.get() }, list(resources, n_resources), list(inputs, n_inputs));
    let out = unsafe { output.get() };
    let len = out.len() as u64;
    let r = catch_unwind(AssertUnwindSafe(|| decode(params, &resources, &inputs, len, threads.max(1))));
    let msg = match r {
        Ok(Ok(got)) if got.len() == out.len() => {
            out.copy_from_slice(&got);
            return 0;
        }
        Ok(Ok(got)) => format!("an output of {} bytes, not {len}", got.len()),
        Ok(Err(e)) => e,
        Err(_) => "panicked".to_owned(),
    };
    unsafe { fail(&msg, error, error_len) }
}

/// The body of `aqr_dll_streamed` ([`StreamedFn`]) of `streamed`.
///
/// # Safety
/// As [`StreamedFn`] is called.
pub unsafe fn streamed(streamed: fn(&[u64], u64) -> bool, lens: *const u64, n_lens: usize, len: u64) -> u32 {
    let lens = match n_lens {
        0 => &[][..],
        n => unsafe { std::slice::from_raw_parts(lens, n) },
    };
    catch_unwind(|| streamed(lens, len)).unwrap_or(false) as u32
}

/// `aqr_dll_info`.
pub type InfoFn = unsafe extern "C" fn() -> Info;

/// `aqr_dll_apply`: the parameters, the resources, the inputs, the outputs (of the sizes
/// declared), and where the message of a failure goes.
pub type ApplyFn = unsafe extern "C" fn(
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const Slice,
    n_inputs: usize,
    outputs: *const SliceMut,
    n_outputs: usize,
    error: SliceMut,
    error_len: *mut usize,
) -> u32;

/// The [`Info`] of the module of binary `name` and entry point `entry`, whose library's
/// `aqr_dll_apply_part` is `apply_part`.
pub fn info(name: &'static str, entry: &Entry, apply_part: ApplyPartFn) -> Info {
    let part = entry.kind == Kind::Part;
    Info {
        version: VERSION,
        part: part as u32,
        arity: entry.arity as u32,
        name: Slice::of(name.as_bytes()),
        apply_part: part.then_some(apply_part),
        // (how it runs natively: the library's to say, by aqr-native)
        run: if part { RUN_PART } else { RUN_WHOLE },
        resources: 1,
        decode: None,
        streamed: None,
    }
}

/// The body of `aqr_dll_apply` ([`ApplyFn`]) of `entry`.
///
/// # Safety
/// As [`ApplyFn`] is called: each pointer to as many [`Slice`]s or [`SliceMut`]s as given,
/// valid as their own safety tells, the outputs and `error` apart from everything else.
#[allow(clippy::too_many_arguments)]
pub unsafe fn apply(
    entry: &Entry,
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const Slice,
    n_inputs: usize,
    outputs: *const SliceMut,
    n_outputs: usize,
    error: SliceMut,
    error_len: *mut usize,
) -> u32 {
    let list = |p: *const Slice, n: usize| match n {
        0 => Vec::new(),
        n => unsafe { std::slice::from_raw_parts(p, n) }.iter().map(|s| unsafe { s.get() }).collect::<Vec<_>>(),
    };
    let outs: &[SliceMut] = match n_outputs {
        0 => &[],
        n => unsafe { std::slice::from_raw_parts(outputs, n) },
    };
    let (params, resources, inputs) = (unsafe { params.get() }, list(resources, n_resources), list(inputs, n_inputs));
    let sizes: Vec<u64> = outs.iter().map(|o| o.len as u64).collect();
    // (a panic past the emulation's own, which must not unwind out of the library)
    let r = catch_unwind(AssertUnwindSafe(|| emu::apply(entry, &inputs, &resources, params, &sizes)));
    let msg = match r {
        Ok(Ok(got)) => {
            for (o, g) in outs.iter().zip(got) {
                unsafe { o.get() }.copy_from_slice(&g);
            }
            return 0;
        }
        Ok(Err(f)) => message(f),
        Err(_) => "panicked".to_owned(),
    };
    unsafe { fail(&msg, error, error_len) }
}

/// Writes `msg` (as much as fits) into `error`, its length into `*error_len`: 1, a failure.
unsafe fn fail(msg: &str, error: SliceMut, error_len: *mut usize) -> u32 {
    let error = unsafe { error.get() };
    let n = msg.len().min(error.len());
    error[..n].copy_from_slice(&msg.as_bytes()[..n]);
    if !error_len.is_null() {
        unsafe { *error_len = n };
    }
    1
}

/// The message of failure `f` of the emulation.
fn message(f: emu::Failure) -> String {
    match f.log {
        Some(log) => format!("{}: {}", f.what, String::from_utf8_lossy(&log)),
        None => f.what,
    }
}

/// A [`PartIn`] as the emulation pulls it.
struct In(PartIn);

impl Feed for In {
    fn len(&self) -> u64 {
        self.0.len
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Result<(), String> {
        unsafe extern "C" fn give(ctx: *mut c_void, ptr: *const u8, len: usize) {
            let put = unsafe { &mut *(ctx as *mut &mut dyn FnMut(&[u8])) };
            put(match len {
                0 => &[],
                n => unsafe { std::slice::from_raw_parts(ptr, n) },
            });
        }
        let mut put = put;
        let ctx = &mut put as *mut &mut dyn FnMut(&[u8]) as *mut c_void;
        // SAFETY: the caller's callback, with its context, `put` alive across the call
        match unsafe { (self.0.pull)(self.0.ctx, hi, ctx, give) } {
            0 => Ok(()),
            _ => Err("an input not pulled".into()),
        }
    }
}

/// A [`PartOut`] as the emulation pushes to it.
struct Out(PartOut);

impl Drain for Out {
    fn push(&mut self, d: &[u8]) -> Result<(), String> {
        // SAFETY: the caller's callback, with its context
        match unsafe { (self.0.push)(self.0.ctx, d.as_ptr(), d.len()) } {
            0 => Ok(()),
            _ => Err("an output not taken".into()),
        }
    }
}

/// The body of `aqr_dll_apply_part` ([`ApplyPartFn`]) of `entry`, a part module's: streamed
/// through [`emu::apply_streaming`], on the calling thread.
///
/// # Safety
/// As [`ApplyPartFn`] is called: each pointer to as many items as given, the callbacks valid
/// with their contexts across the call.
#[allow(clippy::too_many_arguments)]
pub unsafe fn apply_part(
    entry: &Entry,
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const PartIn,
    n_inputs: usize,
    outputs: *const PartOut,
    n_outputs: usize,
    sizes: *const u64,
    error: SliceMut,
    error_len: *mut usize,
) -> u32 {
    let items = |p: *const u8, n: usize| -> bool { n == 0 || !p.is_null() };
    if !items(resources.cast(), n_resources) || !items(inputs.cast(), n_inputs) || !items(outputs.cast(), n_outputs) {
        return unsafe { fail("null items", error, error_len) };
    }
    let slice = |p: *const Slice, n: usize| match n {
        0 => Vec::new(),
        n => unsafe { std::slice::from_raw_parts(p, n) }.iter().map(|s| unsafe { s.get() }).collect::<Vec<_>>(),
    };
    let resources = slice(resources, n_resources);
    let mut ins: Vec<In> = match n_inputs {
        0 => Vec::new(),
        n => unsafe { std::slice::from_raw_parts(inputs, n) }.iter().map(|&p| In(p)).collect(),
    };
    let mut outs: Vec<Out> = match n_outputs {
        0 => Vec::new(),
        n => unsafe { std::slice::from_raw_parts(outputs, n) }.iter().map(|&p| Out(p)).collect(),
    };
    let sizes: &[u64] = match n_outputs {
        0 => &[],
        n => unsafe { std::slice::from_raw_parts(sizes, n) },
    };
    let params = unsafe { params.get() };
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut ins: Vec<&mut dyn Feed> = ins.iter_mut().map(|i| i as &mut dyn Feed).collect();
        let mut outs: Vec<&mut dyn Drain> = outs.iter_mut().map(|o| o as &mut dyn Drain).collect();
        emu::apply_streaming(entry, &mut ins, &resources, params, &mut outs, sizes)
    }));
    let msg = match r {
        Ok(Ok(())) => return 0,
        Ok(Err(f)) => message(f),
        Err(_) => "panicked".to_owned(),
    };
    unsafe { fail(&msg, error, error_len) }
}
