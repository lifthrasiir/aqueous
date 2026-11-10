//! Journal points (`-j`, `--at`, `--history`; `docs/append-design.md`, "Journaling"): a state
//! named by a run, returned to by a number, a label or a date, also where the run changed
//! nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-journal-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// 2026-11-01 12:00 UTC and a day later each, as the clock of a run.
fn day(n: u64) -> String {
    (1_793_534_400_000 + n * 86_400_000).to_string()
}

fn aqr_at(dir: &Path, now: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"));
    c.current_dir(dir).args(args).stdin(Stdio::null());
    if let Some(now) = now {
        c.env("AQR_TEST_NOW", now);
    }
    c.output().unwrap()
}

fn ok(dir: &Path, now: Option<&str>, args: &[&str]) -> Output {
    let o = aqr_at(dir, now, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The paths `aqr -t -q` lists, sorted.
fn listed(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = stdout(&ok(dir, None, args)).lines().map(str::to_owned).collect();
    v.sort();
    v
}

#[test]
fn points_and_states() {
    let d = scratch("states");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), "alpha ".repeat(500)).unwrap();
    // day 0: a.txt
    ok(&d, Some(&day(0)), &["-q", "-j", "first {today}", "-o", "a.aqr", "src"]);
    // day 1: b.txt added
    fs::write(src.join("b.txt"), "beta ".repeat(500)).unwrap();
    ok(&d, Some(&day(1)), &["-q", "-a", "-j", "second", "a.aqr", "src"]);
    // day 2: nothing changed, named all the same
    let o = ok(&d, Some(&day(2)), &["-a", "-j", "unchanged", "a.aqr", "src"]);
    assert!(stderr(&o).contains("journal point"), "{}", stderr(&o));
    // day 3: c.txt added, a point not asked for (nothing named)
    fs::write(src.join("c.txt"), "gamma ".repeat(500)).unwrap();
    ok(&d, Some(&day(3)), &["-q", "-a", "a.aqr", "src"]);

    let h = stdout(&ok(&d, None, &["--history", "a.aqr"]));
    let rows: Vec<&str> = h.lines().collect();
    assert_eq!(rows.len(), 3, "{h}");
    assert!(rows[0].starts_with("#1  ") && rows[0].ends_with("first 2026-11-01"), "{h}");
    assert!(rows[1].ends_with("second") && rows[2].ends_with("unchanged"), "{h}");

    let all = ["a.txt", "b.txt", "c.txt"];
    assert_eq!(listed(&d, &["-t", "-q", "a.aqr"]), all);
    let upto = |at: &str| listed(&d, &["-t", "-q", "--at", at, "a.aqr"]);
    assert_eq!(upto("#1"), ["a.txt"]);
    assert_eq!(upto("@first 2026-11-01"), ["a.txt"]);
    assert_eq!(upto("2026-11-01"), ["a.txt"]);
    assert_eq!(upto("#2"), ["a.txt", "b.txt"]);
    assert_eq!(upto("@second"), ["a.txt", "b.txt"]);
    assert_eq!(upto("2026-11-02"), ["a.txt", "b.txt"]);
    // the third point is the state of the second, named again; c.txt is later
    assert_eq!(upto("#3"), ["a.txt", "b.txt"]);
    assert_eq!(upto("#-1"), ["a.txt", "b.txt"]);
    assert_eq!(upto("2030"), ["a.txt", "b.txt"]);
    // the other readings, with the listing in full
    let full = stdout(&ok(&d, None, &["-t", "--at", "#1", "a.aqr"]));
    assert!(full.contains("a.txt") && !full.contains("b.txt"), "{full}");

    // extracted as it was
    ok(&d, None, &["-q", "-d", "--at", "#1", "-o", "out1", "a.aqr"]);
    assert_eq!(fs::read(d.join("out1/a.txt")).unwrap(), "alpha ".repeat(500).as_bytes());
    assert!(!d.join("out1/b.txt").exists());
    ok(&d, None, &["-q", "-d", "--at", "#2", "-o", "out2", "a.aqr"]);
    assert!(d.join("out2/b.txt").exists() && !d.join("out2/c.txt").exists());
    ok(&d, None, &["-q", "-K", "--at", "#1", "a.aqr"]);
    // and as it is now
    ok(&d, None, &["-q", "-d", "-o", "out3", "a.aqr"]);
    assert!(d.join("out3/c.txt").exists());
}

#[test]
fn asking_what_is_not_there() {
    let d = scratch("errors");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), "alpha ".repeat(100)).unwrap();
    // no point was asked for
    ok(&d, None, &["-q", "-o", "plain.aqr", "src"]);
    for args in [["--history", "plain.aqr"].as_slice(), &["-t", "-q", "--at", "#1", "plain.aqr"]] {
        let o = aqr_at(&d, None, args);
        assert!(!o.status.success() && stderr(&o).contains("no journal points"), "{args:?}: {}", stderr(&o));
    }
    ok(&d, Some(&day(5)), &["-q", "-j", "only", "-o", "j.aqr", "src"]);
    for (at, what) in [("#2", "it has 1"), ("@other", "no point labeled"), ("2026-10", "the earliest is"), ("#0", "--at")] {
        let o = aqr_at(&d, None, &["-t", "-q", "--at", at, "j.aqr"]);
        assert!(!o.status.success() && stderr(&o).contains(what), "{at}: {}", stderr(&o));
    }
    // not where it has no meaning
    for args in [
        ["-j", "x", "-d", "j.aqr"].as_slice(),
        &["-j", "x", "-t", "j.aqr"],
        &["--at", "#1", "-o", "x.aqr", "src"],
        &["-j", "{tomorrow}", "-o", "x.aqr", "src"],
        &["-j", "x", "--format", "zip", "-o", "x.zip", "src"],
        &["-j", "x", "--compact", "j.aqr"],
    ] {
        let o = aqr_at(&d, None, args);
        assert!(!o.status.success(), "{args:?} succeeded");
        assert!(!d.join("x.aqr").exists() && !d.join("x.zip").exists());
    }
    // an archive of a single file has no segments to name
    fs::write(d.join("one.txt"), "x").unwrap();
    let o = aqr_at(&d, None, &["-q", "-j", "x", "-o", "one.aqr", "one.txt"]);
    assert!(!o.status.success() && stderr(&o).contains("single file"), "{}", stderr(&o));
    // writing it again would lose the points
    let o = aqr_at(&d, None, &["--compact", "j.aqr"]);
    assert!(!o.status.success() && stderr(&o).contains("journal"), "{}", stderr(&o));
}
