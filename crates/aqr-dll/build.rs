//! The module of `AQR_DLL_MODULE` (a binary of aqr-modules, `lz4-recons`): its source, without
//! the inner attributes and doc comments that `include!` does not take, in `$OUT_DIR/module.rs`
//! with its name, and `cfg(aqr_dll_module)`. Without it, no module (the library exports nothing).
//!
//! Its `#[global_allocator]` is left out too: the allocator is the shared runtime's
//! (`aqr-dll-rt`, layout.py), the system's, which is what aqr-api's `Stack` is natively.

use std::path::Path;

fn main() {
    println!("cargo::rerun-if-env-changed=AQR_DLL_MODULE");
    println!("cargo::rerun-if-changed=shared.rs");
    println!("cargo::rustc-check-cfg=cfg(aqr_dll_module)");
    let out = Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("module.rs");
    let Some(name) = std::env::var("AQR_DLL_MODULE").ok().filter(|n| !n.is_empty()) else {
        std::fs::write(&out, "").unwrap();
        return;
    };
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aqr-modules/src/bin").join(format!("{name}.rs"));
    println!("cargo::rerun-if-changed={}", src.display());
    let text = std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
    // (the name of its module: known.rs's entry whose block is the binary's, as a module's name
    // may have a / for the binary's -)
    let known_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aqr-modules/src/known.rs");
    println!("cargo::rerun-if-changed={}", known_rs.display());
    let known = std::fs::read_to_string(&known_rs).unwrap_or_else(|e| panic!("{}: {e}", known_rs.display()));
    let block = format!("/{name}.block\")");
    let module_name = known
        .split("Known {")
        .find(|e| e.contains(&block))
        .and_then(|e| e.split("name: \"").nth(1)?.split('"').next())
        .unwrap_or_else(|| panic!("{name}: not in {}", known_rs.display()));
    let mut module = format!(
        "/// The binary of aqr-modules built.\npub const NAME: &str = {name:?};\n\n\
         /// The name of its module.\npub const MODULE: &str = {module_name:?};\n\n"
    );
    let mut allocator = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("#[global_allocator]") {
            allocator = true;
        } else if allocator {
            // (the item after it: its static)
            allocator = !t.ends_with(';');
        } else if !t.starts_with("#![") && !t.starts_with("//!") {
            module.push_str(line);
            module.push('\n');
        }
    }
    std::fs::write(&out, module).unwrap();
    println!("cargo::rustc-cfg=aqr_dll_module");
}
