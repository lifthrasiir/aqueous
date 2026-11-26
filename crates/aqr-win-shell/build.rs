//! The libraries of Windows 8 on that the `windows` crate links (`combase.dll` of windows-core,
//! `api-ms-win-core-winrt-error-l1-1-0.dll` of windows-result) delay-loaded, for the library
//! (and its tests) to load on Windows 7, where the hook of `src/delay.rs` gives `ole32.dll` for
//! the one and a function of no effect for the other.

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|o| o == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if windows && msvc {
        for dll in ["combase.dll", "api-ms-win-core-winrt-error-l1-1-0.dll"] {
            println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-lib=delayimp");
    }
}
