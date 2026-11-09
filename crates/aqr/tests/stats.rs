//! `--stats` given alone, with no input nor any other option: `--stats=FILE.html` writes the page
//! with no data (where a file of `--stats=FILE.json` is opened), and the rest tell that nothing
//! happened.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NOTHING: &str = "Nothing happened, which is just as it should be.";

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-stats-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn stats_alone() {
    let d = scratch("alone");
    let out = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .args(["--stats=page.html", "--stats=data.json", "--stats"])
        .current_dir(&d)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.lines().any(|l| l == NOTHING), "{err}");
    assert_eq!(fs::read_to_string(d.join("page.html")).unwrap(), include_str!("../src/stats.html"));
    let json = fs::read_to_string(d.join("data.json")).unwrap();
    assert_eq!(json, format!("{{\"message\":\"{NOTHING}\"}}\n"));
    fs::remove_dir_all(&d).unwrap();
}

/// Bare `--stats` alone tells it on standard error, writing nothing.
#[test]
fn bare_stats_alone() {
    let d = scratch("bare");
    let out = Command::new(env!("CARGO_BIN_EXE_aqr")).arg("--stats").current_dir(&d).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stderr), format!("{NOTHING}\n"));
    assert!(fs::read_dir(&d).unwrap().next().is_none());
    fs::remove_dir_all(&d).unwrap();
}

/// With any other option, it is a run of aqr wanting its inputs as ever (listing: compressing,
/// standard input would be its input).
#[test]
fn stats_with_another_option() {
    let d = scratch("other");
    let out =
        Command::new(env!("CARGO_BIN_EXE_aqr")).args(["-t", "--stats=page.html"]).current_dir(&d).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no input"));
    assert!(fs::read_dir(&d).unwrap().next().is_none());
    fs::remove_dir_all(&d).unwrap();
}
