//! `--run-before CMD`, `--run-after CMD`: commands run by the system's shell before and after each
//! archive is written, the archive in `AQR_OUTPUT`; one failing fails aqr; an archive a command
//! left unfound fails; not with what reads archives alone.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    common::scratch("hooks", name)
}

/// `aqr -q` with `args`, run in directory `dir`: whether it succeeded, and its standard error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).arg("-q").args(args).output();
    let o = o.unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// A command of the system's shell appending `AQR_HOOK`, `AQR_OUTPUT` and `tag` as a line to
/// `log.txt` (of the working directory).
fn logging(tag: &str) -> String {
    if cfg!(windows) {
        format!("echo %AQR_HOOK% {tag} %AQR_OUTPUT%>>log.txt")
    } else {
        format!("echo \"$AQR_HOOK {tag} $AQR_OUTPUT\" >>log.txt")
    }
}

/// The lines of `log.txt` in `d`.
fn log(d: &Path) -> Vec<String> {
    fs::read_to_string(d.join("log.txt")).unwrap_or_default().lines().map(|l| l.trim_end().to_string()).collect()
}

fn inputs(d: &Path) {
    fs::create_dir_all(d.join("in")).unwrap();
    fs::write(d.join("in/a.txt"), common::text(1, 20_000)).unwrap();
}

/// Each before the archive is written, then each after, in the order given, the archive's
/// absolute path in their environment; of `-c`, `-a`, an archive of another format and an edit.
#[test]
fn before_and_after() {
    let d = scratch("order");
    inputs(&d);
    let (b1, b2, a1) = (logging("b1"), logging("b2"), logging("a1"));
    let ok = aqr_in(&d, &["-c", "-o", "a.aqr", "--run-before", &b1, "--run-after", &a1, "--run-before", &b2, "in"]);
    assert!(ok.0, "{}", ok.1);
    let out = std::path::absolute(d.join("a.aqr")).unwrap().display().to_string();
    assert_eq!(log(&d), [format!("before b1 {out}"), format!("before b2 {out}"), format!("after a1 {out}")]);
    fs::remove_file(d.join("log.txt")).unwrap();
    // appended to, an archive of another format, edited in place
    fs::write(d.join("b.txt"), b"b").unwrap();
    let ok = aqr_in(&d, &["-a", "a.aqr", "b.txt", "--run-after", &a1]);
    assert!(ok.0, "{}", ok.1);
    let ok = aqr_in(&d, &["-c", "-o", "a.zip", "--run-after", &a1, "in"]);
    assert!(ok.0, "{}", ok.1);
    let ok = aqr_in(&d, &["--compact", "a.aqr", "--run-before", &b1, "--run-after", &a1]);
    assert!(ok.0, "{}", ok.1);
    let zip = std::path::absolute(d.join("a.zip")).unwrap().display().to_string();
    let want =
        [format!("after a1 {out}"), format!("after a1 {zip}"), format!("before b1 {out}"), format!("after a1 {out}")];
    assert_eq!(log(&d), want);
    fs::remove_dir_all(&d).unwrap();
}

/// A command failing fails aqr: before, nothing written; after, the archive left as it is.
#[test]
fn failing() {
    let d = scratch("failing");
    inputs(&d);
    let (ok, err) = aqr_in(&d, &["-c", "-o", "a.aqr", "--run-before", "exit 3", "in"]);
    assert!(!ok && err.contains("--run-before \"exit 3\" exited with 3"), "{err}");
    assert!(!d.join("a.aqr").exists());
    let (ok, err) = aqr_in(&d, &["-c", "-o", "a.aqr", "--run-after", "exit 4", "in"]);
    assert!(!ok && err.contains("--run-after \"exit 4\" exited with 4"), "{err}");
    assert!(d.join("a.aqr").exists());
    // (the next not run once one failed)
    let a1 = logging("a1");
    let (ok, _) = aqr_in(&d, &["-c", "-f", "-o", "a.aqr", "--run-after", "exit 4", "--run-after", &a1, "in"]);
    assert!(!ok && log(&d).is_empty());
    fs::remove_dir_all(&d).unwrap();
}

/// An Aqueous archive that a command after left aqr unable to find in its file fails aqr.
#[test]
fn archive_lost() {
    let d = scratch("lost");
    inputs(&d);
    let spoil = if cfg!(windows) { "echo spoiled>\"%AQR_OUTPUT%\"" } else { "echo spoiled >\"$AQR_OUTPUT\"" };
    let (ok, err) = aqr_in(&d, &["-c", "-o", "a.aqr", "--run-after", spoil, "in"]);
    assert!(!ok && err.contains("no archive found in it after --run-after"), "{err}");
    // (bytes added after it, as a transfer would: found still)
    let add = if cfg!(windows) { "echo more>>\"%AQR_OUTPUT%\"" } else { "echo more >>\"$AQR_OUTPUT\"" };
    let (ok, err) = aqr_in(&d, &["-c", "-f", "-o", "a.aqr", "--run-after", add, "in"]);
    assert!(ok, "{err}");
    fs::remove_dir_all(&d).unwrap();
}

/// Not with what reads archives alone, nor an archive to standard output.
#[test]
fn of_archives_written() {
    let d = scratch("refused");
    inputs(&d);
    assert!(aqr_in(&d, &["-c", "-o", "a.aqr", "in"]).0);
    for (args, what) in [
        (&["-d", "a.aqr", "--run-after", "x"][..], "--run-after with -d"),
        (&["-K", "a.aqr", "--run-before", "x"], "--run-before with -d or -K"),
        (&["-t", "a.aqr", "--run-after", "x"], "--run-after with -t"),
        (&["-I", "a.aqr", "--run-after", "x"], "--run-after with -I"),
        (&["-c", "-o", "-", "in", "--run-after", "x"], "standard output"),
    ] {
        let (ok, err) = aqr_in(&d, args);
        assert!(!ok && err.contains(what), "{args:?}: {err}");
    }
    fs::remove_dir_all(&d).unwrap();
}
