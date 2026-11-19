//! The self-extracting stub: an Aqueous archive is appended to this executable, and running it
//! decodes the archive and writes its files out. The modules run through the blobs merged into
//! this stub, or placed in memory from the bundle of blobs after it: the native blobs of the
//! modules that the archive names by their hashes, and the interpreter blob (`aqr-interp`) for any
//! other, so the stub links no codecs and no Wasmtime; the apply logic is in [`aqr_sfx`].
//!
//! What follows the stub's image in its file (`aqr --format sfx`, the stub packed by UPX) is a
//! bundle of blobs ([`aqr_sfx::bundle`]), if any, then the archive, which other bytes may follow
//! (a signature: the archive is located, spec "Locating an archive"). A stub with the blobs merged into it (`aqr-ld merge`) has no bundle; where no archive
//! opens right after the image, the stub scans itself from the end for the last place a valid
//! archive begins, by the archive's header magic.
//!
//! `aqr-sfx-shim [DIR]` writes the files under `DIR` (the working directory by default).

use std::path::PathBuf;
use std::process::ExitCode;

use aqr_archive::HEADER_MAGIC;
use aqr_sfx::{Sfx, extract};

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
    let mut runner = Sfx::find();
    let mut at = overlay(&bytes).unwrap_or(bytes.len());
    if let Some(n) = aqr_sfx::bundle::len(&bytes[at..]) {
        runner.load(&bytes[at..at + n])?;
        at += n;
    }

    // the appended archive starts right after the image (and the bundle), and may be followed by
    // other bytes (a signature); else find where it begins by its header magic, scanning from the
    // end and taking the last place a valid archive opens
    let opened = |x: usize| {
        let d = &bytes[x..];
        aqr_archive::open(&d[aqr_archive::locate(d).filter(|r| r.start == 0)?], &runner).ok()
    };
    let archive = match opened(at) {
        Some(a) => a,
        None => (0..=bytes.len().saturating_sub(HEADER_MAGIC.len()))
            .rev()
            .filter(|&x| bytes[x..].starts_with(&HEADER_MAGIC))
            .find_map(opened)
            .ok_or("no Aqueous archive is appended to this stub")?,
    };

    let dest: PathBuf = std::env::args_os().nth(1).map_or_else(|| PathBuf::from("."), PathBuf::from);
    let value = archive.value(runner.threads()).map_err(|e| format!("{e:?}"))?;
    let n = extract(value, &dest)?;
    eprintln!("aqr-sfx: {n} file{} written under {}", if n == 1 { "" } else { "s" }, dest.display());
    Ok(())
}

/// Where the PE image `exe` ends in its file (its last section's raw data; what follows is its
/// overlay), `None` where it is not one.
fn overlay(exe: &[u8]) -> Option<usize> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(exe.get(o..o + 2)?.try_into().ok()?) as usize);
    let u32_at = |o: usize| Some(u32::from_le_bytes(exe.get(o..o + 4)?.try_into().ok()?) as usize);
    let pe = u32_at(0x3c)?;
    if exe.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let (count, optional) = (u16_at(pe + 6)?, u16_at(pe + 20)?);
    let table = pe + 24 + optional;
    let mut end = table + 40 * count;
    for s in (0..count).map(|i| table + 40 * i) {
        let (size, at) = (u32_at(s + 16)?, u32_at(s + 20)?);
        if size > 0 {
            end = end.max(at + size);
        }
    }
    (end <= exe.len()).then_some(end)
}
