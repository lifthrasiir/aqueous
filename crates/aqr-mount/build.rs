//! Delay-loads WinFsp's DLL, so that the program starts where WinFsp is not installed (and
//! says so). winfsp-sys links the import library and `delayimp`, but a dependency's
//! `rustc-link-arg` reaches only its own targets: the binary's `/DELAYLOAD` is said here.

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        return;
    }
    let dll = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default().as_str() {
        "x86_64" => "winfsp-x64.dll",
        "x86" => "winfsp-x86.dll",
        "aarch64" => "winfsp-a64.dll",
        arch => panic!("WinFsp has no DLL for {arch}"),
    };
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
}
