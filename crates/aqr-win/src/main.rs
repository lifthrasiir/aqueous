//! Aqueous in Explorer: verbs on files, folders and archives, and a window of how far the work
//! is, the work done by `aqr --ipc` (`crates/aqr/src/ipc.rs`) in a process of its own.
//!
//! usage: aqr-win --register | --unregister | compress[-options|-passphrase|-zip|-7z] PATH... |
//!        extract ARCHIVE... | test ARCHIVE... | name-version ARCHIVE | extract-version ARCHIVE |
//!        rollback-version ARCHIVE #N
//!
//! - `--register` adds the verbs for the user (`register`): "Compress with Aqueous" on files
//!   and folders, and the menu "Aqueous" beside it with the rest ("Compress with passphrase…",
//!   which encrypts: `aqr -P`; then "Compress into .zip" and ".7z", `aqr --format zip`, `7z`);
//!   "Extract here" (also the double-click) and "Test" on `.aqr` files and the other archives
//!   `aqr` extracts. `--unregister` takes them away.
//! - `--install` and `--uninstall [--quiet]`, of the program that aqr-win-install installed
//!   (`install`): the former run by the installer, the latter by Settings' Apps.
//! - `compress-options`: the dialog of the options first (`options`), `aqr --session` beside it
//!   finding out how many files and how big the paths are; Compress then has that `aqr` make the
//!   archive, as `compress` does. `options-preview` shows the dialog alone, on examples.
//! - `compress`: the paths into an archive, a single one into `PATH.aqr`, several into one
//!   named after their folder (`compress-zip`: `.zip`, `compress-7z`: `.7z`,
//!   `register::OTHER_FORMATS`). `extract`: each archive beside it, a single entry at its top
//!   as it is, else into its name without `.aqr` (`aqr --each --auto-dir`). `test`: each archive
//!   checked, and what it holds told.
//! - `name-version` and `extract-version`, on one `.aqr` file chosen (`version`): "Name this
//!   version…" names the state the archive is in (`aqr --tag`), and "Extract a previous
//!   version…" extracts one of the versions named (the points of its journal: `aqr -j`, `--tag`)
//!   as it was, into a folder beside the archive named after it (`extract-version ARCHIVE #N`:
//!   point N, asked for nothing: the Versions tab of the shell extension's).
//! - `rollback-version ARCHIVE #N` (the Versions tab's "Delete later versions…"): the versions
//!   after point N deleted (`aqr --rollback`), or point N saved as a new archive instead (`aqr
//!   --convert --keep-until`), as a dialog asks (`version`).
//! - `add ARCHIVE DIR PATH...`: the paths added to the archive under `DIR` in it (`aqr -a`,
//!   each `--rename DIR/NAME`), as files are dropped or pasted into its folder in Explorer
//!   (the shell extension's).
//! - Explorer drops the paths chosen on the verb's class (`drop`: `--drop VERB`, this program as
//!   a COM local server), all at once. A verb's command, one process for each path as other
//!   programs may run it, is gathered into the first process of the verb in a folder (`gather`).
//! - `--serve`: the archives that Explorer browses in its windows (a shell extension), served
//!   as a COM local server over `aqr --serve` (`serve`).
//! - `aqr.exe` is the one beside this program, else the one on `PATH` (`AQR` names another).
//!
//! The window is a task dialog (`dialog`): what is being done, a bar of how far, the file at
//! work and the time left, and a button that stops it (the output being written removed). An
//! output there already is asked about; an error is shown with what `aqr` said.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod dialog;
#[cfg(windows)]
mod dpi;
#[cfg(windows)]
mod drop;
#[cfg(windows)]
mod gather;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod options;
#[cfg(windows)]
mod passphrase;
#[cfg(windows)]
mod register;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod serve;
#[cfg(windows)]
mod version;

#[cfg(not(windows))]
fn main() {
    eprintln!("aqr-win: for Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::path::PathBuf;
    use std::process::ExitCode;

    use aqr_ipc::Verb;
    use aqr_l10n::win;

    aqr_l10n::init(&aqr_l10n::system_languages());
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let verb = args.first().and_then(|a| a.to_str()).unwrap_or("");
    let fail = |main: &str, e: &str| {
        dialog::message(dialog::Icon::Error, main, e, None);
        ExitCode::FAILURE
    };
    // (`--drop VERB`, and COM's `-Embedding`: the paths dropped by Explorer, all at once)
    // (`--serve`: COM's `-Embedding` after it; the archives a shell extension browses)
    if verb == "--serve" {
        return serve::serve(&args[1..]);
    }
    let (verb, paths, dropped) = match verb {
        "--drop" => {
            let v = args.get(1).and_then(|a| a.to_str()).unwrap_or("");
            match drop::receive(v) {
                Ok(paths) => (v, paths, true),
                Err(e) => return fail(win::nothing(), &e),
            }
        }
        _ => (verb, args.iter().skip(1).map(PathBuf::from).collect(), false),
    };
    let (mut encrypt, mut format, mut options) = (false, "aqr", false);
    let name = verb;
    let verb = match verb {
        "--register" => {
            return match register::register() {
                Ok(()) => {
                    dialog::message(dialog::Icon::Information, win::registered(), &register::told(), None);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(win::not_registered(), &e),
            };
        }
        "--unregister" => {
            return match register::unregister() {
                Ok(()) => {
                    dialog::message(dialog::Icon::Information, win::unregistered(), "", None);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(win::not_unregistered(), &e),
            };
        }
        "--install" => {
            return match install::install() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            };
        }
        "--uninstall" => {
            let quiet = match args.get(1).and_then(|a| a.to_str()) {
                None => false,
                Some("--quiet") if args.len() == 2 => true,
                _ => return fail(win::not_uninstall_option(), "aqr-win --uninstall [--quiet]"),
            };
            return match install::uninstall(quiet) {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) if quiet => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
                Err(e) => fail(win::not_uninstalled(), &e),
            };
        }
        "options-preview" => return options::preview(),
        // (the shell extension's drops: ARCHIVE, the directory in it, the paths)
        "add" => {
            let (Some(archive), Some(dir)) = (args.get(1), args.get(2).and_then(|d| d.to_str())) else {
                return fail(win::nothing(), win::no_path());
            };
            let paths: Vec<PathBuf> = args.iter().skip(3).map(PathBuf::from).collect();
            if paths.is_empty() {
                return fail(win::nothing(), win::no_path());
            }
            return run::run_add(std::path::Path::new(archive), dir, &paths);
        }
        "compress-options" => {
            options = true;
            Verb::Compress
        }
        "compress" => Verb::Compress,
        "compress-passphrase" => {
            encrypt = true;
            Verb::Compress
        }
        v if let Some((f, _)) =
            v.strip_prefix("compress-").and_then(|f| register::OTHER_FORMATS.iter().find(|&&(g, _)| g == f)) =>
        {
            format = f;
            Verb::Compress
        }
        "extract" => Verb::Extract,
        "test" => Verb::Test,
        // (one archive: a verb that takes a single selection)
        "name-version" | "extract-version" | "rollback-version" => {
            // (and of extract-version, the number of the point, `#N`, where it is chosen; of
            // rollback-version, always)
            let (archive, number) = match <[PathBuf; 2]>::try_from(paths) {
                Ok([a, n]) if name != "name-version" => {
                    match n.to_str().and_then(|n| n.strip_prefix('#')).and_then(|n| n.parse::<u64>().ok()) {
                        Some(n) => (a, Some(n)),
                        None => return fail(win::not_verb(), &n.to_string_lossy()),
                    }
                }
                Ok(_) => return fail(win::nothing(), win::no_path()),
                Err(paths) => match <[PathBuf; 1]>::try_from(paths) {
                    Ok([a]) => (a, None),
                    Err(_) => return fail(win::nothing(), win::no_path()),
                },
            };
            return match (name, number) {
                ("name-version", _) => version::name(archive),
                ("rollback-version", Some(n)) => version::rollback(archive, n),
                ("rollback-version", None) => fail(win::nothing(), win::no_path()),
                _ => version::extract(archive, number),
            };
        }
        _ => {
            let usage = "aqr-win --register | --unregister | compress[-options|-passphrase|-zip|-7z] PATH... | \
                         extract ARCHIVE... | test ARCHIVE... | name-version ARCHIVE | extract-version ARCHIVE [#N] | rollback-version ARCHIVE #N | add ARCHIVE DIR PATH...";
            return fail(win::not_verb(), usage);
        }
    };
    let paths = match <[PathBuf; 1]>::try_from(paths) {
        Ok([path]) if dropped => vec![path],
        // (one a process, as Explorer runs it: gathered into one)
        Ok([path]) => match gather::gather(name, path) {
            Some(paths) => paths,
            None => return ExitCode::SUCCESS,
        },
        Err(paths) if paths.is_empty() => return fail(win::nothing(), win::no_path()),
        Err(paths) => paths,
    };
    match options {
        true => run::run_options(paths),
        false => run::run(verb, paths, encrypt, format),
    }
}
