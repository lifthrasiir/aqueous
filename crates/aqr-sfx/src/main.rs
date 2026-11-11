//! The self-extracting stub: an Aqueous archive is appended to this executable with a trailer,
//! and running it decodes the archive and writes its files out.
//!
//! The trailer is the last 16 bytes: the archive's length (`u64` little-endian) and the magic
//! [`MAGIC`]; the archive is the bytes before it. `aqr-sfx` with no archive appended prints how
//! it is made.
//!
//! `aqr-sfx [DIR]` writes the files under `DIR` (the working directory by default).
//!
//! The modules are run by [`aqr_decompress::Native`]; an archive that names them by their
//! registered hashes (shorthand module blocks) carries only the hashes, this stub the modules.
//! (A leaner stub carrying only the modules it needs, merged in as native code, is the next step.)

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aqr_formula::aqr::{EntryNode, Value};

/// The last 8 bytes of a stub with an archive appended.
const MAGIC: &[u8; 8] = b"AQRSFX01";

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
    let archive = archive_of(&bytes).ok_or(
        "no archive is appended to this stub. Make a self-extracting archive by appending an \
         Aqueous archive, its length (8 bytes, little-endian) and the magic \"AQRSFX01\".",
    )?;

    let dest: PathBuf = std::env::args_os().nth(1).map_or_else(|| PathBuf::from("."), PathBuf::from);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let value = aqr_decompress::decompress_value(archive, threads).map_err(|e| format!("{e:?}"))?;
    let n = extract(value, &dest)?;
    eprintln!("aqr-sfx: {n} file{} written under {}", if n == 1 { "" } else { "s" }, dest.display());
    Ok(())
}

/// The archive appended to `bytes` (before its trailer), if any.
fn archive_of(bytes: &[u8]) -> Option<&[u8]> {
    let n = bytes.len();
    if n < 16 || &bytes[n - 8..] != MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(bytes[n - 16..n - 8].try_into().ok()?) as usize;
    bytes.get(n - 16 - len..n - 16)
}

/// Writes the files of `value` under `dest`, returning how many were written.
fn extract(value: Value, dest: &Path) -> Result<usize, String> {
    match value {
        Value::File(data) => {
            std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            let path = dest.join("aqr-sfx-output");
            write(&path, &data)?;
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
