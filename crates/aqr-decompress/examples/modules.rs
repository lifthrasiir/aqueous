//! Prints every module of aqr-decompress, a line each: its name, its version and the name of
//! its binary in aqr-modules (`/` as `-`), for crates/aqr-modules/build.sh. Given files of Wasm
//! modules, writes the data of their module blocks next to each instead (`.block`).

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        for m in aqr_decompress::MODULES {
            println!("{} {} {}", m.name, m.version, m.name.replace('/', "-"));
        }
    }
    for f in files {
        let wasm = std::fs::read(&f).unwrap_or_else(|e| panic!("{f}: {e}"));
        let block = aqr_formula::aqr::module_block_data(&wasm).expect("zstd");
        let out = std::path::Path::new(&f).with_extension("block");
        std::fs::write(&out, block).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    }
}
