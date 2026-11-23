//! `wat2wasm FILE.wat...`: each WebAssembly text file made into the module beside it
//! (`FILE.wasm`), by the `wat` crate the tests use. For the conformance suite's modules
//! (`conformance/cases/data/*.wat`), whose binaries are committed with them.

fn main() {
    let files: Vec<_> = std::env::args_os().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: wat2wasm FILE.wat...");
        std::process::exit(2);
    }
    let mut failed = false;
    for f in files {
        let path = std::path::Path::new(&f);
        match wat::parse_file(path) {
            Ok(wasm) => {
                let out = path.with_extension("wasm");
                if let Err(e) = std::fs::write(&out, &wasm) {
                    eprintln!("{}: {e}", out.display());
                    failed = true;
                } else {
                    println!("{}: {} bytes", out.display(), wasm.len());
                }
            }
            Err(e) => {
                eprintln!("{e}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
