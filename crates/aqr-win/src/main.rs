//! Aqueous in Explorer: verbs on files, folders and archives, and a window of how far the work
//! is, the work done by `aqr --ipc` (`crates/aqr/src/ipc.rs`) in a process of its own.
//!
//! usage: aqr-win --register | --unregister | compress[-passphrase|-zip|-7z] PATH... |
//!        extract ARCHIVE... | test ARCHIVE... | name-version ARCHIVE | extract-version ARCHIVE
//!
//! - `--register` adds the verbs for the user (`register`): "Compress with Aqueous" on files
//!   and folders, and the menu "Aqueous" beside it with the rest ("Compress with passphrase…",
//!   which encrypts: `aqr -P`; then "Compress into .zip" and ".7z", `aqr --format zip`, `7z`);
//!   "Extract here" (also the double-click) and "Test" on `.aqr` files and the other archives
//!   `aqr` extracts. `--unregister` takes them away.
//! - `--install` and `--uninstall [--quiet]`, of the program that aqr-win-install installed
//!   (`install`): the former run by the installer, the latter by Settings' Apps.
//! - `compress`: the paths into an archive, a single one into `PATH.aqr`, several into one
//!   named after their folder (`compress-zip`: `.zip`, `compress-7z`: `.7z`,
//!   `register::OTHER_FORMATS`). `extract`: each archive beside it, a single entry at its top
//!   as it is, else into its name without `.aqr` (`aqr --each --auto-dir`). `test`: each archive
//!   checked, and what it holds told.
//! - `name-version` and `extract-version`, on one `.aqr` file chosen (`version`): "Name this
//!   version…" names the state the archive is in (`aqr --tag`), and "Extract a previous
//!   version…" extracts one of the versions named (the points of its journal: `aqr -j`, `--tag`)
//!   as it was, into a folder beside the archive named after it.
//! - Explorer runs a process for each of the paths chosen: those of a verb in a folder are
//!   gathered into the first of them (`gather`).
//! - `aqr.exe` is the one beside this program, else the one on `PATH` (`AQR` names another).
//!
//! The window is a task dialog (`dialog`): what is being done, a bar of how far, the file at
//! work and the time left, and a button that stops it (the output being written removed). An
//! output there already is asked about; an error is shown with what `aqr` said.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod dialog;
#[cfg(windows)]
mod gather;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod passphrase;
#[cfg(windows)]
mod register;
#[cfg(windows)]
mod run;
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
    let paths: Vec<PathBuf> = args.iter().skip(1).map(PathBuf::from).collect();
    let fail = |main: &str, e: &str| {
        dialog::message(dialog::Icon::Error, main, e, None);
        ExitCode::FAILURE
    };
    let (mut encrypt, mut format) = (false, "aqr");
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
        "name-version" | "extract-version" => {
            let Ok([archive]) = <[PathBuf; 1]>::try_from(paths) else { return fail(win::nothing(), win::no_path()) };
            return match name {
                "name-version" => version::name(archive),
                _ => version::extract(archive),
            };
        }
        _ => {
            let usage = "aqr-win --register | --unregister | compress[-passphrase|-zip|-7z] PATH... | \
                         extract ARCHIVE... | test ARCHIVE... | name-version ARCHIVE | extract-version ARCHIVE";
            return fail(win::not_verb(), usage);
        }
    };
    let paths = match <[PathBuf; 1]>::try_from(paths) {
        // (one a process, as Explorer runs it: gathered into one)
        Ok([path]) => match gather::gather(name, path) {
            Some(paths) => paths,
            None => return ExitCode::SUCCESS,
        },
        Err(paths) if paths.is_empty() => return fail(win::nothing(), win::no_path()),
        Err(paths) => paths,
    };
    run::run(verb, paths, encrypt, format)
}
