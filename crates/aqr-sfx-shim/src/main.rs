//! The self-extracting stub: an Aqueous archive is appended to this executable, and running it
//! decodes the archive and writes its files out. The modules run through the blobs merged into
//! this stub, or placed in memory from the bundle of blobs after it: the native blobs of the
//! modules that the archive names by their hashes, and the interpreter blob (`aqr-interp`) for any
//! other, so the stub links no codecs and no Wasmtime; the apply logic is in [`aqr_sfx`].
//!
//! What follows the stub's image in its file (`aqr --format sfx`, the stub packed by UPX) is a
//! bundle of blobs ([`aqr_sfx::bundle`]), if any, the program's settings
//! ([`aqr_sfx::config`]: what it does once run, compiled from the archive's `.aqr/sfx/sfx.toml`),
//! if any, then the archive, which other bytes may follow (a signature: the archive is located,
//! spec "Locating an archive"). A stub with the blobs merged into it (`aqr-ld merge`) has no
//! bundle; where no archive opens right after the image, the stub scans itself from the end for
//! the last place a valid archive begins, by the archive's header magic, its settings right
//! before it.
//!
//! `aqr-sfx-shim [-y] [--noexec] [--keep] [DIR]` writes the files under `DIR` (else where the
//! settings tell: the working directory where there are none), and runs the program the
//! settings tell, waiting for it and cleaning up after it as they tell. `-y` asks nothing (a
//! file there written over where the settings would ask), `--noexec` runs nothing, `--keep`
//! leaves the files written where they would be cleaned up.

#[cfg(windows)]
mod banner;
mod run;
mod shortcut;
mod ui;

use std::path::{Path, PathBuf};

use aqr_archive::HEADER_MAGIC;
use aqr_sfx::config::{Config, Overwrite};
use aqr_sfx::{Extracting, Sfx, clean, extract_with};
use ui::Ui;

const USAGE: &str = "[-y] [--noexec] [--keep] [DIR]\n\
    \x20 DIR       where the files go (else where the program tells)\n\
    \x20 -y        ask nothing: a file there written over where it would be asked\n\
    \x20 --noexec  run nothing once the files are written\n\
    \x20 --keep    leave the files where they would be removed once the program ran";

/// The command line.
#[derive(Default)]
struct Args {
    dest: Option<PathBuf>,
    yes: bool,
    noexec: bool,
    keep: bool,
}

fn args() -> Result<Option<Args>, String> {
    let mut a = Args::default();
    let mut options = true;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--") if options => options = false,
            Some("-y" | "--yes") if options => a.yes = true,
            Some("--noexec") if options => a.noexec = true,
            Some("--keep") if options => a.keep = true,
            Some("-h" | "--help" | "/?") if options => return Ok(None),
            Some(o) if options && o.starts_with('-') && o.len() > 1 => return Err(format!("{o}: not an option")),
            _ if a.dest.is_none() => a.dest = Some(PathBuf::from(arg)),
            _ => return Err("one destination at most".into()),
        }
    }
    Ok(Some(a))
}

fn main() {
    let mut ui = Ui::new();
    let code = match run(&mut ui) {
        Ok(code) => code,
        Err(e) => {
            ui.error(&e);
            1
        }
    };
    std::process::exit(code);
}

/// Runs the program, returning its exit code.
fn run(ui: &mut Ui) -> Result<i32, String> {
    let Some(args) = args()? else {
        ui.tell(&format!("{} {USAGE}", program_name()));
        return Ok(0);
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the program: {e}"))?;
    let bytes = std::fs::read(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let mut runner = Sfx::find();
    let mut at = overlay(&bytes).unwrap_or(bytes.len());
    if let Some(n) = aqr_sfx::bundle::len(&bytes[at..]) {
        runner.load(&bytes[at..at + n])?;
        at += n;
    }
    let mut config = match Config::at(&bytes[at..]) {
        Some(c) => {
            let (c, n) = c?;
            at += n;
            Some(c)
        }
        None => None,
    };

    // the appended archive starts right after the image (and the bundle and the settings), and
    // may be followed by other bytes (a signature); else find where it begins by its header
    // magic, scanning from the end and taking the last place a valid archive opens (its settings
    // right before it)
    let opened = |x: usize| {
        let d = &bytes[x..];
        let c = &d[aqr_archive::locate(d).filter(|r| r.start == 0)?];
        aqr_archive::open(c, &runner).ok().map(|a| (a, x, c))
    };
    let (archive, start, c) = match opened(at) {
        Some(a) => a,
        None => (0..=bytes.len().saturating_sub(HEADER_MAGIC.len()))
            .rev()
            .filter(|&x| bytes[x..].starts_with(&HEADER_MAGIC))
            .find_map(opened)
            .ok_or("no Aqueous archive is appended to this stub")?,
    };
    if config.is_none()
        && let Some(c) = Config::before(&bytes, start)
    {
        config = Some(c?.0);
    }
    let settings = config.is_some();
    let config = config.unwrap_or_default();
    let lang = ui::language();
    ui.title = config.title.as_ref().map_or_else(program_name, |t| t.pick(&lang).to_string());
    ui.quiet = args.yes || config.quiet;

    // the license and the banner (in windows alone), of the archive's first segment
    // (docs/sfx-config-design.md, 2.1), of the user's language where they have one
    let asking = !ui.quiet && (config.prompt.is_some() || config.license.is_some());
    let names_of = |f: &Option<String>, shown: bool| match f {
        Some(f) if shown => aqr_sfx::config::names_for(f, &lang),
        _ => Vec::new(),
    };
    let license = names_of(&config.license, asking);
    let banner = names_of(&config.banner, asking && ui.windowed());
    let mut files = if license.is_empty() && banner.is_empty() {
        Default::default()
    } else {
        let wanted = |n: &str| license.iter().chain(&banner).any(|m| m == n);
        aqr_sfx::config::read_files(c, &runner, runner.threads(), &wanted)?
    };
    ui.banner = banner.iter().find_map(|n| files.remove(n));

    if let Some(p) = &config.prompt
        && !ui.quiet
        && !ui.prompt(p.pick(&lang))
    {
        return Ok(2);
    }
    if let Some(l) = &config.license
        && !ui.quiet
    {
        let text = license
            .iter()
            .find_map(|n| files.get(n))
            .ok_or_else(|| format!("no {l} in the archive's first segment"))?;
        let text = String::from_utf8_lossy(text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text));
        if !ui.license(&text) {
            return Ok(2);
        }
    }
    let fresh = std::cell::RefCell::new(None);
    let dest = match &args.dest {
        Some(d) => d.clone(),
        None => {
            let mut d = aqr_sfx::config::destination(&config.dest, &mut |p| place(p, &fresh))?;
            if config.subdir {
                d.push(aqr_sfx::sanitized(program_name().as_bytes()));
            }
            d
        }
    };
    let value = archive.value(runner.threads()).map_err(|e| format!("{e:?}"))?;
    let mut ask = |p: &Path| ui.overwrite(p);
    let overwrite = match config.overwrite {
        Overwrite::Ask if args.yes || config.quiet => Overwrite::Always,
        o => o,
    };
    let running = config.run.as_ref().filter(|_| !args.noexec);
    let mut x = Extracting { overwrite, skip_aqr: running.is_some(), ..Extracting::new(&mut ask) };
    // (a new temporary directory is what was made, to be cleaned up with the rest)
    if let Some(t) = fresh.borrow_mut().take() {
        x.made.push(t);
    }
    let written = extract_with(value, &dest, &mut x);
    let (left, made) = (x.left, std::mem::take(&mut x.made));
    let cleaning = running.is_some_and(|r| r.cleanup) && !args.keep;
    let n = match written {
        Ok(n) => n,
        Err(e) => {
            if cleaning {
                clean(&made);
            }
            return Err(e);
        }
    };
    // the shortcuts, to the files written
    for s in &config.shortcuts {
        let at = shortcut::make(&dest, s, &lang)?;
        ui.note(&format!("shortcut {}", at.display()));
    }
    let mut code = 0;
    match running {
        None => {
            if !ui.quiet || !settings {
                let left = if left > 0 { format!(" ({left} there left as they were)") } else { String::new() };
                ui.note(&format!("{n} file{} written under {}{left}", if n == 1 { "" } else { "s" }, dest.display()));
            }
        }
        Some(r) => {
            let ended = run::run(&dest, r);
            if cleaning && clean(&made) > 0 {
                ui.note(&format!("{}: not all removed (a file still open)", dest.display()));
            }
            let ended = ended?;
            if r.exit_code {
                code = ended.unwrap_or(0);
            }
        }
    }
    if let Some(f) = &config.finish
        && !ui.quiet
    {
        ui.tell(f.pick(&lang));
    }
    Ok(code)
}

/// The name of this program, its file's without the extension.
fn program_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "aqr-sfx".into())
}

/// The place of destinations `p` ([`aqr_sfx::config::PLACES`]); of `temp` a new directory made,
/// noted in `fresh`.
fn place(p: &str, fresh: &std::cell::RefCell<Option<PathBuf>>) -> Result<PathBuf, String> {
    match p {
        "cwd" => std::env::current_dir().map_err(|e| format!("the working directory: {e}")),
        "here" => std::env::current_exe()
            .map_err(|e| format!("cannot find the program: {e}"))
            .map(|e| e.parent().map(Path::to_path_buf).unwrap_or_default()),
        "programs" => std::env::var_os("ProgramFiles").map(PathBuf::from).ok_or_else(|| "no %ProgramFiles%".into()),
        "temp" => {
            let base = std::env::temp_dir();
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            for k in 0..100u32 {
                let t =
                    base.join(format!("aqr-sfx-{}-{:x}", std::process::id(), nanos as u32 ^ k.wrapping_mul(0x9e37)));
                match std::fs::create_dir(&t) {
                    Ok(()) => {
                        *fresh.borrow_mut() = Some(t.clone());
                        return Ok(t);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(format!("{}: {e}", t.display())),
                }
            }
            Err(format!("{}: no new directory made in it", base.display()))
        }
        _ => Err(format!("{{{p}}}: not a place known")),
    }
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
