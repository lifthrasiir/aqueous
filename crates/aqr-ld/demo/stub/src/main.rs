//! The stub of the PE-merge validation (crates/aqr-ld/pemerge-test.sh): a normal executable
//! that, instead of loading a module's library, reads a dispatch table merged into its own image
//! by `aqr-ld merge` and calls the module's `aqr_mod_apply` through it. Before the merge the
//! table is empty, so `list` shows nothing and `run` fails: the modules are turned on and off by
//! which sections the merge put in.
//!
//! - `list` prints the names of the modules merged in.
//! - `run NAME IN OUT` applies module `NAME` to the bytes of `IN`, writing its output to `OUT`.

use std::process::ExitCode;

/// What `aqr-ld merge` fills: the RVA and length of the dispatch table in the image. Found by
/// its magic (volatile, so the zeros it is built with are not assumed to stay).
#[repr(C)]
struct Anchor {
    magic: [u8; 16],
    dispatch_rva: u32,
    dispatch_len: u32,
}

#[used]
#[unsafe(no_mangle)]
static AQR_PEMERGE_ANCHOR: Anchor =
    Anchor { magic: *b"\x89AQRPEMERGEv1\x00\x00\x00", dispatch_rva: 0, dispatch_len: 0 };

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

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// The modules merged in, from the dispatch table:
/// `count: u32`, then `count` × `{ apply_rva: u32, name_off: u32, name_len: u32 }`, then names.
fn modules() -> Vec<Module> {
    let (rva, len) = unsafe {
        (
            std::ptr::read_volatile(&AQR_PEMERGE_ANCHOR.dispatch_rva),
            std::ptr::read_volatile(&AQR_PEMERGE_ANCHOR.dispatch_len),
        )
    };
    if rva == 0 || len < 4 {
        return Vec::new();
    }
    let table = unsafe { at(rva, len as usize) };
    let count = u32_at(table, 0) as usize;
    (0..count)
        .map(|i| {
            // entry: apply_rva, info_rva, pdata_rva, pdata_count, mod_base, name_off (6 u32)
            let e = 4 + i * 24;
            let apply_rva = u32_at(table, e);
            let off = u32_at(table, e + 20) as usize;
            let nlen = u32_at(table, off) as usize;
            Module {
                name: String::from_utf8_lossy(&table[off + 4..off + 4 + nlen]).into_owned(),
                // SAFETY: the merge wrote the RVA of the module's aqr_mod_apply
                apply: unsafe { std::mem::transmute::<*const u8, ApplyFn>(base().add(apply_rva as usize)) },
            }
        })
        .collect()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mods = modules();
    match args.first().map(String::as_str) {
        Some("list") => {
            for m in &mods {
                println!("{}", m.name);
            }
            ExitCode::SUCCESS
        }
        Some("run") if args.len() == 4 => {
            let Some(m) = mods.iter().find(|m| m.name == args[1]) else {
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
            eprintln!("usage: stub list | stub run NAME IN OUT");
            ExitCode::FAILURE
        }
    }
}
