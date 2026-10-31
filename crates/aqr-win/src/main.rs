//! Aqueous in Explorer: verbs on files, folders and archives, and a window of how far the work
//! is, the work done by `aqr --ipc` (`crates/aqr/src/ipc.rs`) in a process of its own.
//!
//! usage: aqr-win --register | --unregister | compress PATH... | extract ARCHIVE... | test ARCHIVE...
//!
//! - `--register` adds the verbs for the user ([`register`]): "Compress with Aqueous" on files
//!   and folders, "Extract here" (also the double-click) and "Test" on `.aqr` files.
//!   `--unregister` takes them away.
//! - `compress`: the paths into an archive, a single one into `PATH.aqr`, several into one
//!   named after their folder. `extract`: each archive into its name without `.aqr`. `test`:
//!   each archive checked, and what it holds told.
//! - Explorer runs a process for each of the paths chosen: those of a verb in a folder are
//!   gathered into the first of them ([`gather`]).
//! - `aqr.exe` is the one beside this program, else the one on `PATH` (`AQR` names another).
//!
//! The window is a task dialog ([`dialog`]): what is being done, a bar of how far, the file at
//! work and the time left, and a button that stops it (the output being written removed). An
//! output there already is asked about; an error is shown with what `aqr` said.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod dialog;
#[cfg(windows)]
mod gather;
#[cfg(windows)]
mod register;
#[cfg(windows)]
mod run;

#[cfg(not(windows))]
fn main() {
    eprintln!("aqr-win: for Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::path::PathBuf;
    use std::process::ExitCode;

    use run::Verb;

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let verb = args.first().and_then(|a| a.to_str()).unwrap_or("");
    let paths: Vec<PathBuf> = args.iter().skip(1).map(PathBuf::from).collect();
    let fail = |main: &str, e: &str| {
        dialog::message(dialog::Icon::Error, main, e, None);
        ExitCode::FAILURE
    };
    let verb = match verb {
        "--register" => {
            return match register::register() {
                Ok(()) => {
                    let done = "\"Compress with Aqueous\" is on files and folders, \"Extract here\" and \"Test\" on \
                                .aqr files (on Windows 11, under \"Show more options\").";
                    dialog::message(dialog::Icon::Information, "Aqueous is registered", done, None);
                    ExitCode::SUCCESS
                }
                Err(e) => fail("Aqueous is not registered", &e),
            };
        }
        "--unregister" => {
            return match register::unregister() {
                Ok(()) => {
                    dialog::message(dialog::Icon::Information, "Aqueous is unregistered", "", None);
                    ExitCode::SUCCESS
                }
                Err(e) => fail("Aqueous is not unregistered", &e),
            };
        }
        "compress" => Verb::Compress,
        "extract" => Verb::Extract,
        "test" => Verb::Test,
        _ => {
            let usage = "aqr-win --register | --unregister | compress PATH... | extract ARCHIVE... | test ARCHIVE...";
            return fail("Not a verb of Aqueous", usage);
        }
    };
    let paths = match <[PathBuf; 1]>::try_from(paths) {
        // (one a process, as Explorer runs it: gathered into one)
        Ok([path]) => match gather::gather(verb.name(), path) {
            Some(paths) => paths,
            None => return ExitCode::SUCCESS,
        },
        Err(paths) if paths.is_empty() => return fail("Nothing to work on", "No path given."),
        Err(paths) => paths,
    };
    run::run(verb, paths)
}
