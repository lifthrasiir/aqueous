//! The self-extracting stub: an Aqueous archive is appended to this executable, and running it
//! decodes the archive and writes its files out. The modules run through the interpreter blob
//! (`aqr-interp`) merged into this stub by `aqr-ld merge`, so the stub links no codecs and no
//! Wasmtime; the apply logic is in [`aqr_sfx`].
//!
//! The archive is found by its own header magic: the stub scans itself from the end for the last
//! place a valid archive begins, so nothing is appended but the archive (which runs to the end of
//! the file).
//!
//! `aqr-sfx-shim [DIR]` writes the files under `DIR` (the working directory by default).

use std::path::PathBuf;
use std::process::ExitCode;

use aqr_formula::aqr::{self, HEADER_MAGIC};
use aqr_sfx::{Interp, extract};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-sfx: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the program: {e}"))?;
    let bytes = std::fs::read(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let runner = Interp::find().ok_or("no interpreter blob is merged into this stub")?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());

    // the appended archive runs to the end of the file; find where it begins by its header magic,
    // scanning from the end and taking the last place a valid archive opens
    let archive = (0..=bytes.len().saturating_sub(HEADER_MAGIC.len()))
        .rev()
        .filter(|&x| bytes[x..].starts_with(&HEADER_MAGIC))
        .find_map(|x| aqr::open(&bytes[x..], &runner).ok())
        .ok_or("no Aqueous archive is appended to this stub")?;

    let dest: PathBuf = std::env::args_os().nth(1).map_or_else(|| PathBuf::from("."), PathBuf::from);
    let value = archive.value(threads).map_err(|e| format!("{e:?}"))?;
    let n = extract(value, &dest)?;
    eprintln!("aqr-sfx: {n} file{} written under {}", if n == 1 { "" } else { "s" }, dest.display());
    Ok(())
}
