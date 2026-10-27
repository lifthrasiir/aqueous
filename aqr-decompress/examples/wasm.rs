//! Decodes archives (`aqr -c` of a file) twice, with the modules run as WebAssembly and
//! natively, checks that both give the same bytes, and prints how long each took. With the
//! feature `sandbox`, a third time as WebAssembly in the sandbox (of the worker built beside
//! the examples: `cargo build --release --bins --examples`).

use aqr_decompress::{Native, Wasm};
use aqr_formula::aqr;
use std::time::Instant;

fn main() {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    #[cfg(feature = "sandbox")]
    let worker = {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(|d| d.parent()).unwrap();
        dir.join("aqr-decompress-worker").with_extension(std::env::consts::EXE_EXTENSION)
    };
    for f in std::env::args().skip(1) {
        let c = std::fs::read(&f).unwrap_or_else(|e| panic!("{f}: {e}"));
        let t = Instant::now();
        let wasm = aqr::decode(&c, &Wasm).unwrap_or_else(|e| panic!("{f}: as WebAssembly: {e:?}"));
        let tw = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let native = aqr::decode(&c, &Native { threads }).unwrap_or_else(|e| panic!("{f}: natively: {e:?}"));
        let tn = t.elapsed().as_secs_f64();
        assert!(wasm == native, "{f}: WebAssembly and native differ");
        let mb = native.len() as f64 / 1e6;
        print!("{f}: {mb:.1} MB, WebAssembly {tw:.3} s ({:.0} MB/s), native {tn:.3} s ({:.0} MB/s)", mb / tw, mb / tn);
        #[cfg(feature = "sandbox")]
        {
            // a sandbox of its own, its worker started in the time
            let t = Instant::now();
            let sandbox = aqr_decompress::sandbox::Sandbox::new(&worker, [""; 0]);
            let s = aqr::decode(&c, &sandbox).unwrap_or_else(|e| panic!("{f}: in the sandbox: {e:?}"));
            let ts = t.elapsed().as_secs_f64();
            assert!(s == native, "{f}: the sandbox and native differ");
            print!(", in the sandbox {ts:.3} s ({:.0} MB/s)", mb / ts);
        }
        println!();
    }
}
