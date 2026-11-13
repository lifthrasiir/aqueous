use std::process::ExitCode;

use aqr_blob::dispatch::{self, Anchor};

#[used]
#[unsafe(no_mangle)]
static AQR_PEMERGE_ANCHOR: Anchor = Anchor::EMPTY;

unsafe extern "C" {
    /// The image base, a symbol the MSVC linker defines.
    static __ImageBase: u8;
}

/// `extern "C" fn(inp, inlen, outp, outlen) -> u32` of a module's `aqr_mod_apply`.
type ApplyFn = unsafe extern "C" fn(*const u8, usize, *mut u8, usize) -> u32;

/// A module in the dispatch table.
struct Module {
    name: String,
    apply: ApplyFn,
}

fn base() -> *const u8 {
    &raw const __ImageBase
}

/// Reads `n` bytes at offset `rva` of the image.
unsafe fn at(rva: u32, n: usize) -> &'static [u8] {
    unsafe { std::slice::from_raw_parts(base().add(rva as usize), n) }
}

/// The modules merged in, from the dispatch table (each image registered for its panics,
/// `aqr_blob::register_image`).
fn modules() -> Vec<Module> {
    aqr_blob::keep_vcruntime();
    let Some((rva, len)) = AQR_PEMERGE_ANCHOR.table() else { return Vec::new() };
    let table = unsafe { at(rva, len as usize) };
    dispatch::entries(table)
        .map(|e| {
            let at = |rva: u32| base() as usize + rva as usize;
            aqr_blob::register_image(at(e.image_rva), e.image_len as usize, at(e.mod_base));
            Module {
                name: String::from_utf8_lossy(e.name).into_owned(),
                // SAFETY: the merge wrote the RVA of the module's aqr_mod_apply
                apply: unsafe { std::mem::transmute::<*const u8, ApplyFn>(base().add(e.apply_rva as usize)) },
            }
        })
        .collect()
}

pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mods = modules();
    match args.first().map(String::as_str) {
        Some("list") => {
            for m in &mods {
                println!("{}", m.name);
            }
            ExitCode::SUCCESS
        }
        Some(cmd @ ("run" | "load")) if args.len() == 4 => {
            let loaded;
            let m = match cmd {
                "run" => mods.iter().find(|m| m.name == args[1]),
                _ => {
                    let blob = std::fs::read(&args[1]).map_err(|e| e.to_string());
                    match blob.and_then(|b| aqr_blob::load(&[&b])) {
                        Ok(image) => {
                            let p = &image.blobs[0];
                            // SAFETY: the blob's entry, aqr_mod_apply (`aqr-ld blob --apply`)
                            let apply = unsafe { std::mem::transmute::<usize, ApplyFn>(p.at(p.apply_rva)) };
                            loaded = Module { name: args[1].clone(), apply };
                            Some(&loaded)
                        }
                        Err(e) => {
                            eprintln!("stub: {}: {e}", args[1]);
                            return ExitCode::FAILURE;
                        }
                    }
                }
            };
            let Some(m) = m else {
                eprintln!("stub: no module {}", args[1]);
                return ExitCode::FAILURE;
            };
            let input = match std::fs::read(&args[2]) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("stub: {}: {e}", args[2]);
                    return ExitCode::FAILURE;
                }
            };
            let mut output = vec![0u8; input.len()];
            let r = unsafe { (m.apply)(input.as_ptr(), input.len(), output.as_mut_ptr(), output.len()) };
            if r != 0 {
                eprintln!("stub: {} failed with {r}", m.name);
                return ExitCode::FAILURE;
            }
            if let Err(e) = std::fs::write(&args[3], &output) {
                eprintln!("stub: {}: {e}", args[3]);
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: stub list | stub run NAME IN OUT | stub load BLOB IN OUT");
            ExitCode::FAILURE
        }
    }
}
