//! Decodes archives (`aqr -c`) with the modules run as WebAssembly, and prints for
//! every application of a module the peak of its working memories (those of its tasks and
//! the messages of its channels included; not the memories of its parameters, resources,
//! inputs and outputs), against the length of its outputs. `-t N` runs the tasks on N threads
//! at most (the calling one and N − 1 workers; by default as many as the machine has).
//!
//! Columns: the module, m (the outputs' bytes), I (the parameters', resources' and inputs'
//! bytes), the peak of the working memories, that peak over m, and the module's estimate of it
//! (`Module::memory_usage_in_pages`, in bytes) over the peak, `!` where it is below the peak.

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
    println!("{:<22} {:>12} {:>12} {:>12} {:>7} {:>7}", "module", "m", "I", "working", "w/m", "est/w");
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
            let known = aqr_modules::KNOWN.iter().find(|k| k.sha512_256 == a.hash);
            let name = known.map_or("(unknown)", |k| k.name);
            let m: u64 = a.sizes.iter().sum();
            let i = a.params + a.resources.iter().sum::<u64>() + a.inputs.iter().sum::<u64>();
            let est = known.and_then(|k| estimate(k, &a));
            let est = match est {
                Some(e) => format!("{:>7.2}{}", e as f64 / a.working as f64, if e < a.working { "!" } else { "" }),
                None => format!("{:>7}", "-"),
            };
            println!(
                "  {:<20} {:>12} {:>12} {:>12} {:>7.2} {est}{}",
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

/// The estimate of the module `k` of what `a` took, in bytes.
fn estimate(k: &aqr_modules::Known, a: &aqr_decompress::Applied) -> Option<u64> {
    let m = aqr_decompress::MODULES.iter().find(|m| m.name == k.name && m.version == k.version)?;
    let contents: Vec<&[u8]> = a.contents.iter().map(|c| &c[..]).collect();
    let usage = aqr_formula::Usage {
        params: &a.params_data,
        resources: &a.resources,
        inputs: &a.inputs,
        outputs: &a.sizes,
        contents: &contents,
        initial_pages: aqr_decompress::initial_pages(&k.read_wasm().ok()?)?,
    };
    Some(m.memory_usage_in_pages(&usage)? * aqr_formula::PAGE)
}
