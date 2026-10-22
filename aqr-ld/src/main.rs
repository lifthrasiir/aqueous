//! Links an aqr module written with `aqr-api`, or rewrites one already linked.
//!
//! - As the linker of `wasm32-unknown-unknown`
//!   (e.g. `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=path/to/aqr-ld`), it runs `rust-lld`
//!   and rewrites the output if it uses `aqr_api`, leaving any other output alone.
//! - `aqr-ld <in.wasm> <out.wasm>` rewrites `in.wasm` into `out.wasm`.

mod rewrite;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = if args.first().is_some_and(|a| a == "-flavor") {
        link(&args)
    } else if let [input, output] = &args[..] {
        process(input.into(), output.into(), true)
    } else {
        Err("usage: aqr-ld <in.wasm> <out.wasm>, or as the linker of wasm32-unknown-unknown".into())
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
    let status = Command::new(&lld).args(args).status().map_err(|e| format!("{}: {e}", lld.display()))?;
    if !status.success() {
        return Err(format!("rust-lld failed with {status}"));
    }
    let output = args.windows(2).find(|w| w[0] == "-o").map(|w| PathBuf::from(&w[1])).ok_or("no -o")?;
    process(output.clone(), output, false)
}

/// Rewrites `input` into `output`; unless `force`, only if it uses `aqr_api`.
fn process(input: PathBuf, output: PathBuf, force: bool) -> Result<(), String> {
    let wasm = std::fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    if !force && !rewrite::is_aqr_module(&wasm)? {
        return Ok(());
    }
    let wasm = rewrite::rewrite(&wasm).map_err(|e| format!("{}: {e}", input.display()))?;
    std::fs::write(&output, wasm).map_err(|e| format!("{}: {e}", output.display()))
}
