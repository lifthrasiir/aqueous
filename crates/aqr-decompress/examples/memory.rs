//! Decodes archives (`aqr -c`) with the modules run as WebAssembly, and prints for
//! every application of a module the peak of its working memories (those of its tasks and
//! the messages of its channels included; not the memories of its parameters, resources,
//! inputs and outputs), against the length of its outputs. `-t N` runs the tasks on N threads
//! at most (the calling one and N − 1 workers; by default as many as the machine has).
//!
//! Columns: the module, m (the outputs' bytes), I (the parameters', resources' and inputs'
//! bytes), the peak of the working memories, that peak over m, and the module's plan of it
//! (`Module::plan_in_pages`, in bytes) over the peak, `!` where it is below the peak;
//! and the memory limit declared over the total (the peak and the memories imported), `-` for
//! none; then the most the application's instance held and what the native measurement
//! (`aqr_common::meter`) took for it, and the most one task held and the native measurement's
//! (a task's own, and with the copy of the instance it starts from).

use aqr_decompress::Wasm;
use aqr_formula::aqr;

// (the allocator that tells aqr-common's meter of the blocks: the native measurement)
#[global_allocator]
static ALLOC: aqr_common::mem::Mapped = aqr_common::mem::Mapped;

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("-t") {
        args.next();
        let t: usize = args.next().and_then(|t| t.parse().ok()).expect("-t N");
        // (the workers of the pool, which poll the tasks with the calling thread)
        aqr_common::pool::init(t.saturating_sub(1).max(1));
    }
    let threads = aqr_common::pool::size() + 1;
    println!(
        "{:<22} {:>12} {:>12} {:>12} {:>7} {:>7} {:>7} {:>11} {:>11} {:>11} {:>11}",
        "module", "m", "I", "working", "w/m", "plan/w", "lim/t", "inst", "n.inst", "task", "n.task"
    );
    for f in args {
        let c = std::fs::read(&f).unwrap_or_else(|e| panic!("{f}: {e}"));
        // the native measurement first ([`aqr_decompress::record_native`]), by the applications'
        // shapes (module, parameters, lengths)
        aqr_decompress::record_native(true);
        let _ = aqr::decode_value(&c, &aqr_decompress::Native { threads });
        let mut native = aqr_decompress::record_native(false);
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
            let est = known.and_then(|k| plan(k, &a));
            let est = match est {
                Some(e) => format!("{:>7.2}{}", e as f64 / a.working as f64, if e < a.working { "!" } else { "" }),
                None => format!("{:>7}", "-"),
            };
            let lim = match a.limit {
                u64::MAX => format!("{:>7}", "-"),
                l => format!("{:>7.2}", l as f64 / (a.working + a.imported) as f64),
            };
            // the native measurement of the same shape, taken once
            let n = native
                .iter()
                .position(|n| {
                    n.module.name == name && n.params == a.params_data && n.inputs == a.inputs && n.sizes == a.sizes
                })
                .map(|k| native.swap_remove(k).measured);
            let (ni, nt) = match n {
                Some(n) => (n.peak.to_string(), format!("{} {}", n.task, n.task_with_copy)),
                None => ("-".into(), "-".into()),
            };
            println!(
                "  {:<20} {:>12} {:>12} {:>12} {:>7.2} {est} {lim} {:>11} {:>11} {:>11} {:>11}{}",
                name,
                m,
                i,
                a.working,
                a.working as f64 / m.max(1) as f64,
                a.instance,
                ni,
                a.task,
                nt,
                if a.ok { "" } else { " failed" }
            );
        }
    }
}

/// The plan of the module `k` of what `a` took, in bytes.
fn plan(k: &aqr_modules::Known, a: &aqr_decompress::Applied) -> Option<u64> {
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
    Some(m.plan_in_pages(&usage)? * aqr_formula::PAGE)
}
