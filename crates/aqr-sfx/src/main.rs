//! The self-extracting stub: an Aqueous archive is appended to this executable, and running it
//! decodes the archive and writes its files out. The modules are run through the interpreter blob
//! (`aqr-interp`) merged into this stub by `aqr-ld merge`, so the stub itself links no codecs and
//! no Wasmtime; the interpreter blob holds Wasmtime and runs each module, sandboxed, found through
//! the dispatch table the merge wrote into the anchor.
//!
//! The archive is found by its own header magic: the stub scans itself from the end for the last
//! place a valid archive begins, so nothing is appended but the archive (which runs to the end of
//! the file). An archive made with a size hint narrows the search.
//!
//! `aqr-sfx [DIR]` writes the files under `DIR` (the working directory by default).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aqr_formula::aqr::{self, EntryNode, HEADER_MAGIC, Value};

mod interp;

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
    let runner = interp::Interp::find().ok_or("no interpreter blob is merged into this stub")?;
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

/// Writes the files of `value` under `dest`, returning how many were written.
fn extract(value: Value, dest: &Path) -> Result<usize, String> {
    match value {
        Value::File(data) => {
            std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            write(&dest.join("aqr-sfx-output"), &data)?;
            Ok(1)
        }
        Value::Tree(tree, contents) => {
            let mut paths: Vec<PathBuf> = Vec::with_capacity(tree.entries.len());
            let mut files = 0;
            for e in &tree.entries {
                // the entries come each after its parent, so the parent's path is known
                let base = match e.parent {
                    Some(p) => paths[p].clone(),
                    None => dest.to_path_buf(),
                };
                let path = base.join(sanitized(&e.name));
                match &e.node {
                    EntryNode::Dir => std::fs::create_dir_all(&path).map_err(|x| format!("{}: {x}", path.display()))?,
                    EntryNode::File(c) => {
                        let data = contents.get(*c).ok_or_else(|| format!("{}: no content", path.display()))?;
                        write(&path, data)?;
                        files += 1;
                    }
                    // links and special entries are left out of this first stub
                    _ => {}
                }
                paths.push(path);
            }
            Ok(files)
        }
    }
}

/// Writes `data` to `path`, making its parent directory.
fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

/// An entry name (WTF-8) as a path component, lossily, with any separators of its own dropped so
/// that it cannot escape its directory.
fn sanitized(name: &[u8]) -> String {
    String::from_utf8_lossy(name).replace(['/', '\\'], "_").replace("..", "_")
}
