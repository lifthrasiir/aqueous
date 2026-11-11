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
//!
//! The library holds a copy of the module's code and of everything it uses, the standard library
//! among them (its own allocator, its own threads for the tasks).

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::emu::{self, Entry, Kind};

/// The version of the interface, which [`Info::version`] tells.
pub const VERSION: u32 = 1;

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

/// The [`Info`] of the module of binary `name` and entry point `entry`.
pub fn info(name: &'static str, entry: &Entry) -> Info {
    Info {
        version: VERSION,
        part: (entry.kind == Kind::Part) as u32,
        arity: entry.arity as u32,
        name: Slice::of(name.as_bytes()),
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
        Ok(Err(f)) => match f.log {
            Some(log) => format!("{}: {}", f.what, String::from_utf8_lossy(&log)),
            None => f.what,
        },
        Err(_) => "panicked".to_owned(),
    };
    let error = unsafe { error.get() };
    let n = msg.len().min(error.len());
    error[..n].copy_from_slice(&msg.as_bytes()[..n]);
    if !error_len.is_null() {
        unsafe { *error_len = n };
    }
    1
}
