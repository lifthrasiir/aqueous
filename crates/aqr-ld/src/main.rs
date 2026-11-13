//! Links an aqr module written with `aqr-api`, or rewrites one already linked.
//!
//! - As the linker of `wasm32-unknown-unknown`
//!   (e.g. `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=path/to/aqr-ld`), it runs `rust-lld`
//!   and rewrites the output if it uses `aqr_api`, leaving any other output alone. It links with
//!   the relocations kept and the symbols not stripped (it strips them itself), for the rewrite
//!   to take out what only panics need; any other output is linked again as asked.
//! - `aqr-ld <in.wasm> <out.wasm>` rewrites `in.wasm` into `out.wasm`.
//! - `aqr-ld blob --out M.blob MODULE.dll` and `aqr-ld blobs --out DIR LIBRARY.dll ...` digest
//!   native libraries into blobs; `aqr-ld exports --out DIR LIBRARY.dll ...` writes for each
//!   library the symbols that the others import from it.
//! - `aqr-ld merge --stub S --out O NAME=MODULE.dll ...` merges native module libraries into a
//!   stub executable as sections, with no loader ([`pemerge`]), toward self-extracting archives.
//!
//! With `AQR_LD_NAMES` set, the name section is kept, to see what is left in a module.

mod msvc;
mod rewrite;
mod trim;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use aqr_ld::pemerge;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = if args.first().is_some_and(|a| a == "-flavor") {
        link(&args)
    } else if args.first().is_some_and(|a| a == "merge") {
        pemerge::main(&args[1..])
    } else if args.first().is_some_and(|a| a == "blob") {
        pemerge::blob_main(&args[1..])
    } else if args.first().is_some_and(|a| a == "blobs") {
        pemerge::blobs_main(&args[1..])
    } else if args.first().is_some_and(|a| a == "exports") {
        msvc::exports_main(&args[1..])
    } else if msvc::is_link(&args) {
        msvc::link(&args)
    } else if let [input, output] = &args[..] {
        process(input.into(), output.into())
    } else {
        Err("usage: aqr-ld <in.wasm> <out.wasm>, aqr-ld blob --out M.blob MODULE.dll, \
             aqr-ld blobs --out DIR LIBRARY.dll ..., \
             aqr-ld merge --stub S --out O NAME=MODULE ..., \
             or as the linker of wasm32-unknown-unknown"
            .into())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("aqr-ld: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn rustc(args: &[&str]) -> Result<String, String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc).args(args).output().map_err(|e| format!("rustc: {e}"))?;
    if !out.status.success() {
        return Err(format!("rustc {args:?} failed"));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn link(args: &[OsString]) -> Result<(), String> {
    let sysroot = rustc(&["--print", "sysroot"])?;
    let version = rustc(&["-vV"])?;
    let host = version.lines().find_map(|l| l.strip_prefix("host: ")).ok_or("no host in rustc -vV")?;
    let lld: PathBuf = [sysroot.trim(), "lib", "rustlib", host, "bin", "rust-lld"].iter().collect();
    let run = |args: &[OsString]| {
        let status = Command::new(&lld).args(args).status().map_err(|e| format!("{}: {e}", lld.display()))?;
        if !status.success() {
            return Err(format!("rust-lld failed with {status}"));
        }
        Ok(())
    };
    let strip = ["--strip-all", "--strip-debug", "-s", "-S"];
    let mut ours: Vec<OsString> = args.iter().filter(|a| !strip.iter().any(|s| *a == s)).cloned().collect();
    ours.push("--emit-relocs".into());
    run(&ours)?;
    let output = args.windows(2).find(|w| w[0] == "-o").map(|w| PathBuf::from(&w[1])).ok_or("no -o")?;
    let wasm = std::fs::read(&output).map_err(|e| format!("{}: {e}", output.display()))?;
    if !rewrite::is_aqr_module(&wasm)? {
        return run(args);
    }
    process(output.clone(), output)
}

/// Rewrites `input` into `output`.
fn process(input: PathBuf, output: PathBuf) -> Result<(), String> {
    let wasm = std::fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let wasm = rewrite::rewrite(&wasm).map_err(|e| format!("{}: {e}", input.display()))?;
    std::fs::write(&output, wasm).map_err(|e| format!("{}: {e}", output.display()))
}
