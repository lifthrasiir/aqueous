//! `aqr_dist` where built for distribution (`--profile dist`, and the Wasm profile inheriting
//! it): development's switches (`AQR_SCAN_DUMP`, `lib.rs`) are left out of such builds.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(aqr_dist)");
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    if out.split(['/', '\\']).any(|c| c == "dist" || c == "release-wasm") {
        println!("cargo::rustc-cfg=aqr_dist");
    }
}
