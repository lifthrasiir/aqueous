//! Embeds `aqr-win.manifest` in the binary (MSVC's linker): common controls 6 (the task
//! dialogs) and per-monitor DPI awareness; and `aqr-win.rc`, the icon (`droplet.ico` at the
//! root).

fn main() {
    println!("cargo:rerun-if-changed=aqr-win.manifest");
    println!("cargo:rerun-if-changed=../../droplet.ico");
    // (nothing done for a target other than Windows)
    embed_resource::compile("aqr-win.rc", embed_resource::NONE).manifest_optional().unwrap();
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|o| o == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if windows && msvc {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("aqr-win.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
}
