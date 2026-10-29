//! Decodes archives (`aqr -c`) with the modules run as WebAssembly, and prints for
//! every application of a module the peak of its working memories (those of its tasks and
//! the messages of its channels included; not the memories of its parameters, resources,
//! inputs and outputs), against the length of its outputs. `-t N` runs the tasks on N threads
//! at most (the calling one and N − 1 workers; by default as many as the machine has).
//!
//! Columns: the module, m (the outputs' bytes), I (the parameters', resources' and inputs'
//! bytes), the peak of the working memories, and that peak over m.

use aqr_decompress::Wasm;
use aqr_formula::aqr;

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("-t") {
        args.next();
        let t: usize = args.next().and_then(|t| t.parse().ok()).expect("-t N");
        // (the workers of the pool, which poll the tasks with the calling thread)
        aqr_common::pool::init(t.saturating_sub(1).max(1));
    }
    println!("{:<22} {:>12} {:>12} {:>12} {:>7}", "module", "m", "I", "working", "w/m");
    for f in args {
        let c = std::fs::read(&f).unwrap_or_else(|e| panic!("{f}: {e}"));
        aqr_decompress::record_applications(true);
        let r = aqr::decode_value(&c, &Wasm);
        let applied = aqr_decompress::record_applications(false);
        match r {
            Ok(_) => println!("{f}"),
            Err(e) => println!("{f} (failed: {e:?})"),
        }
        for a in applied {
            let name = aqr_modules::KNOWN.iter().find(|k| k.sha512_256 == a.hash).map_or("(unknown)", |k| k.name);
            let m: u64 = a.sizes.iter().sum();
            let i = a.params + a.resources.iter().sum::<u64>() + a.inputs.iter().sum::<u64>();
            println!(
                "  {:<20} {:>12} {:>12} {:>12} {:>7.2}{}",
                name,
                m,
                i,
                a.working,
                a.working as f64 / m.max(1) as f64,
                if a.ok { "" } else { " failed" }
            );
        }
    }
}
