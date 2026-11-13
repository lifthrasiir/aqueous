//! The stub of the PE-merge validation (crates/aqr-ld/pemerge-test.sh): a normal executable
//! that, instead of loading a module's library, reads a dispatch table merged into its own image
//! by `aqr-ld merge` and calls the module's `aqr_mod_apply` through it. Before the merge the
//! table is empty, so `list` shows nothing and `run` fails: the modules are turned on and off by
//! which sections the merge put in.
//!
//! - `list` prints the names of the modules merged in.
//! - `run NAME IN OUT` applies module `NAME` to the bytes of `IN`, writing its output to `OUT`.
//! - `load BLOB IN OUT` does the same with the module of blob `BLOB` (`aqr-ld blob`), placed in
//!   memory by `aqr_blob::load` rather than merged.

#[cfg(windows)]
mod stub;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    stub::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("stub: for Windows only");
    std::process::exit(1);
}
