//! A module of aqr-modules (`AQR_DLL_MODULE`, the name of its binary) built as a native library,
//! exporting the interface of `aqr_api::dll`: `build.sh` builds one for each module into
//! `target/aqr-dll/`. The binary's source is taken as it is (`build.rs`), its entry point
//! `apply` applied under aqr-api's emulation; and how the module runs natively is aqr-native's
//! entry of its name ([`NATIVE`]: its native decoding, `aqr_dll_decode`), only that entry's code
//! linked. The crates that other modules use too are in the shared libraries it links against
//! (layout.py: the dylibs of shared/, the runtime among them).

#[cfg(aqr_dll_module)]
include!(concat!(env!("OUT_DIR"), "/module.rs"));

// (the shared libraries, layout.py's: what of them the module uses is taken from them)
#[cfg(aqr_dll_module)]
include!("../shared.rs");

/// How the module runs natively (aqr-native's entry of its name: one an aqr-modules binary has).
#[cfg(aqr_dll_module)]
pub const NATIVE: aqr_native::Native = match aqr_native::find(MODULE) {
    Some(n) => n,
    None => panic!("a module with no entry in aqr-native"),
};

/// Its native decoding of whole inputs, if it has one.
#[cfg(aqr_dll_module)]
const DECODE: Option<aqr_native::Decode> = match NATIVE.run {
    aqr_native::Run::Whole(d)
    | aqr_native::Run::Part(_, Some(d))
    | aqr_native::Run::Threaded(d)
    | aqr_native::Run::Streamed(_, d, _) => Some(d),
    aqr_native::Run::Part(_, None) => None,
};

/// # Safety
/// As `aqr_api::dll::DecodeFn` is called.
#[cfg(aqr_dll_module)]
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn aqr_dll_decode(
    params: aqr_api::dll::Slice,
    resources: *const aqr_api::dll::Slice,
    n_resources: usize,
    inputs: *const aqr_api::dll::Slice,
    n_inputs: usize,
    output: aqr_api::dll::SliceMut,
    threads: usize,
    error: aqr_api::dll::SliceMut,
    error_len: *mut usize,
) -> u32 {
    let decode = DECODE.unwrap_or(|_, _, _, _, _| Err("no native decoding".into()));
    unsafe {
        aqr_api::dll::decode(
            decode,
            params,
            resources,
            n_resources,
            inputs,
            n_inputs,
            output,
            threads,
            error,
            error_len,
        )
    }
}

/// # Safety
/// As `aqr_api::dll::StreamedFn` is called.
#[cfg(aqr_dll_module)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_dll_streamed(lens: *const u64, n_lens: usize, len: u64) -> u32 {
    match NATIVE.run {
        aqr_native::Run::Streamed(_, _, streamed) => unsafe { aqr_api::dll::streamed(streamed, lens, n_lens, len) },
        _ => 0,
    }
}

/// # Safety
/// As `aqr_api::dll::ApplyPartFn` is called.
#[cfg(aqr_dll_module)]
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn aqr_dll_apply_part(
    params: aqr_api::dll::Slice,
    resources: *const aqr_api::dll::Slice,
    n_resources: usize,
    inputs: *const aqr_api::dll::PartIn,
    n_inputs: usize,
    outputs: *const aqr_api::dll::PartOut,
    n_outputs: usize,
    sizes: *const u64,
    error: aqr_api::dll::SliceMut,
    error_len: *mut usize,
) -> u32 {
    unsafe {
        aqr_api::dll::apply_part(
            &apply::ENTRY,
            params,
            resources,
            n_resources,
            inputs,
            n_inputs,
            outputs,
            n_outputs,
            sizes,
            error,
            error_len,
        )
    }
}

#[cfg(aqr_dll_module)]
#[unsafe(no_mangle)]
pub extern "C" fn aqr_dll_info() -> aqr_api::dll::Info {
    use aqr_api::dll::{RUN_PART, RUN_STREAMED, RUN_THREADED, RUN_WHOLE};
    let (run, streamed) = match NATIVE.run {
        aqr_native::Run::Whole(_) => (RUN_WHOLE, None),
        aqr_native::Run::Part(..) => (RUN_PART, None),
        aqr_native::Run::Threaded(_) => (RUN_THREADED, None),
        aqr_native::Run::Streamed(..) => (RUN_STREAMED, Some(aqr_dll_streamed as aqr_api::dll::StreamedFn)),
    };
    aqr_api::dll::Info {
        run,
        resources: NATIVE.resources as u32,
        decode: DECODE.map(|_| aqr_dll_decode as aqr_api::dll::DecodeFn),
        streamed,
        ..aqr_api::dll::info(NAME, &apply::ENTRY, aqr_dll_apply_part)
    }
}

/// # Safety
/// As `aqr_api::dll::ApplyFn` is called.
#[cfg(aqr_dll_module)]
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn aqr_dll_apply(
    params: aqr_api::dll::Slice,
    resources: *const aqr_api::dll::Slice,
    n_resources: usize,
    inputs: *const aqr_api::dll::Slice,
    n_inputs: usize,
    outputs: *const aqr_api::dll::SliceMut,
    n_outputs: usize,
    error: aqr_api::dll::SliceMut,
    error_len: *mut usize,
) -> u32 {
    unsafe {
        aqr_api::dll::apply(
            &apply::ENTRY,
            params,
            resources,
            n_resources,
            inputs,
            n_inputs,
            outputs,
            n_outputs,
            error,
            error_len,
        )
    }
}
