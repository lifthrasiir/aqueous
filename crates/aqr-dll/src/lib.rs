//! A module of aqr-modules (`AQR_DLL_MODULE`, the name of its binary) built as a native library,
//! exporting the interface of `aqr_api::dll`: `build.sh` builds one for each module into
//! `target/aqr-dll/`. The binary's source is taken as it is (`build.rs`), its entry point
//! `apply` applied under aqr-api's emulation. The crates that other modules use too are in the
//! shared libraries it links against (layout.py: the dylibs of shared/, the runtime among them).

#[cfg(aqr_dll_module)]
include!(concat!(env!("OUT_DIR"), "/module.rs"));

// (the shared libraries, layout.py's: what of them the module uses is taken from them)
#[cfg(aqr_dll_module)]
include!("../shared.rs");

#[cfg(aqr_dll_module)]
#[unsafe(no_mangle)]
pub extern "C" fn aqr_dll_info() -> aqr_api::dll::Info {
    aqr_api::dll::info(NAME, &apply::ENTRY)
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
