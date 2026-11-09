//! Standard input and output as an input or output, `-`: through files of a temporary
//! directory of this process ([`Temp`]), as an archive is mapped and read back (verified, its
//! footer patched as it is committed) rather than streamed. Standard input is read whole into a
//! file there before the work begins; an archive made for standard output is made whole there
//! (`--atomic`) and copied out once it is.
//!
//! The directory is removed when the work is done, and when the process is stopped
//! ([`crate::stop::on_end`]). What is told of the files there names them `-`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Whether `p`, as given, is standard input or output.
pub fn is(p: &Path) -> bool {
    p.as_os_str() == "-"
}

/// The temporary directory of this process, where made.
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The temporary directory of this process, made where it is not.
fn dir() -> Result<PathBuf, String> {
    let mut d = DIR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = &*d {
        return Ok(d.clone());
    }
    let base = std::env::temp_dir();
    let made = (0..100).find_map(|k| {
        let p = base.join(format!("aqr-{}-{k}", std::process::id()));
        std::fs::create_dir(&p).ok().map(|()| p)
    });
    let p = made.ok_or(format!("{}: no directory made for standard input or output", base.display()))?;
    crate::stop::on_end(remove);
    *d = Some(p.clone());
    Ok(p)
}

/// Removes the temporary directory, where made.
pub fn remove() {
    if let Some(d) = DIR.lock().unwrap_or_else(|e| e.into_inner()).take() {
        _ = std::fs::remove_dir_all(d);
    }
}

/// The paths of the temporary directory that stand for standard input and output, to be told
/// as `-`.
static SHOWN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// `p` as it is told: `-` where it stands for standard input or output.
pub fn shown(p: &Path) -> std::path::Display<'_> {
    match SHOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|s| s == p) {
        true => Path::new("-").display(),
        false => p.display(),
    }
}

/// A file of the temporary directory standing for standard input or output, of name `name`
/// (not made).
pub fn temp(name: &str) -> Result<PathBuf, String> {
    let p = dir()?.join(name);
    SHOWN.lock().unwrap_or_else(|e| e.into_inner()).push(p.clone());
    Ok(p)
}

/// Standard input read whole into a file of the temporary directory: its path.
pub fn spool_in() -> Result<PathBuf, String> {
    let p = temp("stdin")?;
    let err = |e: std::io::Error| format!("standard input: {e}");
    let mut f = std::fs::File::create_new(&p).map_err(err)?;
    std::io::copy(&mut std::io::stdin().lock(), &mut f).map_err(err)?;
    f.sync_data().ok();
    Ok(p)
}

/// Copies file `p` to standard output.
pub fn copy_out(p: &Path) -> Result<(), String> {
    let err = |e: std::io::Error| format!("standard output: {e}");
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut out = std::io::stdout().lock();
    std::io::copy(&mut f, &mut out).map_err(err)?;
    out.flush().map_err(err)
}

/// Whether the archive was written to standard output as it was made (not to be copied there).
static STREAMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_streamed() {
    STREAMED.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn streamed() -> bool {
    STREAMED.load(std::sync::atomic::Ordering::SeqCst)
}
