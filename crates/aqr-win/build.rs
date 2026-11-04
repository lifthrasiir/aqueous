//! Embeds `aqr-win.manifest` in the binary (MSVC's linker): common controls 6 (the task
//! dialogs) and per-monitor DPI awareness; and `aqr-win.rc`, the icons (`droplet.ico`, from
//! `droplet.svg` at the root; `icons/*.ico`, the files', from `scripts/icons.py`) and the
//! version (aqr-version, also `env!("AQR_VERSION")`); and Windows' string table of aqr-l10n's
//! catalogs (`strings.rc`, what Explorer reads of the registry's indirect strings).

fn main() {
    println!("cargo:rerun-if-changed=aqr-win.manifest");
    println!("cargo:rerun-if-changed=droplet.ico");
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=aqr-win.rc");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    // (not rewritten unchanged, so that rc.exe's input keeps its time)
    if std::fs::read_to_string(out.join("strings.rc")).ok().as_deref() != Some(aqr_l10n::RC) {
        std::fs::write(out.join("strings.rc"), aqr_l10n::RC).unwrap();
    }
    let version = aqr_version::emit();
    aqr_version::write_rc(&version, "Aqueous in Explorer", "aqr-win.exe");
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
