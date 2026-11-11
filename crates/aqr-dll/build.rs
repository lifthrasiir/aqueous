//! The module of `AQR_DLL_MODULE` (a binary of aqr-modules, `lz4-recons`): its source, without
//! the inner attributes and doc comments that `include!` does not take, in `$OUT_DIR/module.rs`
//! with its name, and `cfg(aqr_dll_module)`. Without it, no module (the library exports nothing).

use std::path::Path;

fn main() {
    println!("cargo::rerun-if-env-changed=AQR_DLL_MODULE");
    println!("cargo::rustc-check-cfg=cfg(aqr_dll_module)");
    let out = Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("module.rs");
    let Some(name) = std::env::var("AQR_DLL_MODULE").ok().filter(|n| !n.is_empty()) else {
        std::fs::write(&out, "").unwrap();
        return;
    };
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aqr-modules/src/bin").join(format!("{name}.rs"));
    println!("cargo::rerun-if-changed={}", src.display());
    let text = std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
    let mut module = format!("/// The binary of aqr-modules built.\npub const NAME: &str = {name:?};\n\n");
    for line in text.lines() {
        let t = line.trim_start();
        if !t.starts_with("#![") && !t.starts_with("//!") {
            module.push_str(line);
            module.push('\n');
        }
    }
    std::fs::write(&out, module).unwrap();
    println!("cargo::rustc-cfg=aqr_dll_module");
}
