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

    // as JSON: the points, none maybe
    let j: serde_json::Value =
        serde_json::from_str(stdout(&ok(&d, None, &["--history", "--json", "a.aqr"])).trim()).unwrap();
    assert_eq!(j["points"].as_array().unwrap().len(), 3);
    assert_eq!(j["points"][0]["number"], 1);
    assert_eq!(j["points"][0]["time"], day(0).parse::<u64>().unwrap());
    assert_eq!(j["points"][0]["label"], "first 2026-11-01");
    assert_eq!(j["points"][2]["label"], "unchanged");
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
    for (at, what) in
        [("#2", "it has 1"), ("@other", "no point labeled"), ("2026-10", "the earliest is"), ("#0", "--at")]
    {
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

/// Some text of `n` bytes that does not compress to nothing.
fn words(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    let mut t = Vec::new();
    while t.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        t.extend_from_slice(format!("{:x} ", x >> 20).as_bytes());
    }
    t.truncate(n);
    t
}

/// What `--at` shows of each point of `archive`: the paths and the bytes of each file.
fn states(dir: &Path, archive: &str, points: usize) -> Vec<Vec<(String, Vec<u8>)>> {
    (1..=points)
        .map(|k| {
            // (a name of its own each time: the states are read of one archive more than once)
            let out = (0..).map(|n| format!("state{k}-{n}-{archive}")).find(|o| !dir.join(o).exists()).unwrap();
            ok(dir, None, &["-q", "-d", "--at", &format!("#{k}"), "-o", &out, archive]);
            let mut v: Vec<(String, Vec<u8>)> = fs::read_dir(dir.join(&out))
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap())
                })
                .collect();
            v.sort();
            v
        })
        .collect()
}

#[test]
fn compacting_keeps_the_points_asked_for() {
    let d = scratch("compact");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), words(1, 20_000)).unwrap();
    ok(&d, Some(&day(0)), &["-q", "-j", "p1", "-o", "a.aqr", "src"]);
    // a.txt replaced, b.txt added
    fs::write(src.join("a.txt"), words(2, 21_000)).unwrap();
    fs::write(src.join("b.txt"), words(3, 5_000)).unwrap();
    ok(&d, Some(&day(1)), &["-q", "-a", "-j", "p2", "a.aqr", "src"]);
    // b.txt taken away
    fs::remove_file(src.join("b.txt")).unwrap();
    ok(&d, Some(&day(2)), &["-q", "-s", "-j", "p3", "a.aqr", "src"]);
    // nothing changed, named again
    ok(&d, Some(&day(3)), &["-q", "-a", "-j", "p4", "a.aqr", "src"]);
    // c.txt added
    fs::write(src.join("c.txt"), words(4, 6_000)).unwrap();
    ok(&d, Some(&day(4)), &["-q", "-a", "-j", "p5", "a.aqr", "src"]);
    let before = states(&d, "a.aqr", 5);
    assert_eq!(before[0].len(), 1);
    assert_eq!(before[1].len(), 2);
    assert_eq!(before[2].len(), 1);
    assert_eq!(before[2], before[3]);
    assert_eq!(before[4].len(), 2);
    let size = fs::metadata(d.join("a.aqr")).unwrap().len();

    // neither said: refused, and left as it was
    let o = aqr_at(&d, None, &["--compact", "a.aqr"]);
    assert!(!o.status.success() && stderr(&o).contains("--keep-since"), "{}", stderr(&o));
    assert_eq!(fs::metadata(d.join("a.aqr")).unwrap().len(), size);

    // from the second point on: the first state gone, the others as they were
    fs::copy(d.join("a.aqr"), d.join("k.aqr")).unwrap();
    ok(&d, None, &["-q", "--compact", "--keep-since", "@p2", "k.aqr"]);
    let h = stdout(&ok(&d, None, &["--history", "k.aqr"]));
    let labels: Vec<&str> = h.lines().map(|l| l.rsplit("  ").next().unwrap()).collect();
    assert_eq!(labels, ["p2", "p3", "p4", "p5"], "{h}");
    assert_eq!(states(&d, "k.aqr", 4), before[1..]);
    ok(&d, None, &["-q", "-K", "k.aqr"]);
    for k in 1..=4 {
        ok(&d, None, &["-q", "-K", "--at", &format!("#{k}"), "k.aqr"]);
    }
    let kept = fs::metadata(d.join("k.aqr")).unwrap().len();
    assert!(kept < size, "{kept} < {size}: the first a.txt is gone");
    // again, by a date: the third point on
    ok(&d, None, &["-q", "--compact", "--keep-since", "2026-11-03", "k.aqr"]);
    let h = stdout(&ok(&d, None, &["--history", "k.aqr"]));
    assert_eq!(h.lines().count(), 3, "{h}");
    assert_eq!(states(&d, "k.aqr", 3)[0], before[2]);

    // none: the last state alone
    fs::copy(d.join("a.aqr"), d.join("n.aqr")).unwrap();
    ok(&d, None, &["-q", "--compact", "--drop-history", "n.aqr"]);
    let o = aqr_at(&d, None, &["--history", "n.aqr"]);
    assert!(!o.status.success() && stderr(&o).contains("no journal points"));
    assert_eq!(listed(&d, &["-t", "-q", "n.aqr"]), ["a.txt", "c.txt"]);
    ok(&d, None, &["-q", "-K", "n.aqr"]);
    assert!(fs::metadata(d.join("n.aqr")).unwrap().len() < kept);

    // not where they have no meaning
    for args in [
        ["--keep-since", "#1", "-t", "a.aqr"].as_slice(),
        &["--drop-history", "-t", "a.aqr"],
        &["--compact", "--keep-since", "#1", "--drop-history", "a.aqr"],
        &["--compact", "--keep-since", "#9", "a.aqr"],
    ] {
        assert!(!aqr_at(&d, None, args).status.success(), "{args:?} succeeded");
    }
}

#[test]
fn diff_between_states() {
    let d = scratch("diff");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), words(1, 3_000)).unwrap();
    ok(&d, Some(&day(0)), &["-q", "-j", "p1", "-o", "a.aqr", "src"]);
    fs::write(src.join("a.txt"), words(2, 3_500)).unwrap();
    fs::write(src.join("b.txt"), words(3, 1_000)).unwrap();
    ok(&d, Some(&day(1)), &["-q", "-a", "-j", "p2", "a.aqr", "src"]);
    fs::remove_file(src.join("b.txt")).unwrap();
    ok(&d, Some(&day(2)), &["-q", "-s", "-j", "p3", "a.aqr", "src"]);
    ok(&d, Some(&day(3)), &["-q", "-a", "-j", "p4", "a.aqr", "src"]);
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub/c.txt"), words(4, 2_000)).unwrap();
    ok(&d, Some(&day(4)), &["-q", "-a", "-j", "p5", "a.aqr", "src"]);

    let diff = |args: &[&str]| -> Vec<String> {
        let mut all = vec!["--diff", "-q"];
        all.extend_from_slice(args);
        all.push("a.aqr");
        stdout(&ok(&d, None, &all)).lines().map(str::to_owned).collect()
    };
    // the last point against the archive now: nothing yet
    assert_eq!(diff(&[]), Vec::<String>::new());
    fs::write(src.join("d.txt"), words(5, 500)).unwrap();
    ok(&d, None, &["-q", "-a", "a.aqr", "src"]);
    assert_eq!(diff(&[]), ["+ d.txt"]);
    // a point against the one before it
    assert_eq!(diff(&["--at", "#1"]), ["+ a.txt"]);
    assert_eq!(diff(&["--at", "#2"]), ["~ a.txt", "+ b.txt"]);
    assert_eq!(diff(&["--at", "#3"]), ["- b.txt"]);
    assert_eq!(diff(&["--at", "#4"]), Vec::<String>::new());
    assert_eq!(diff(&["--at", "@p5"]), ["+ sub/", "+ sub/c.txt"]);
    // any two
    assert_eq!(diff(&["--from", "#1", "--at", "#5"]), ["~ a.txt", "+ sub/", "+ sub/c.txt"]);
    assert_eq!(diff(&["--from", "@p2", "--at", "2026-11-03"]), ["- b.txt"]);
    assert_eq!(diff(&["--from", "#5"]), ["+ d.txt"]);
    assert_eq!(diff(&["--from", "#2", "--at", "#1"]), ["~ a.txt", "- b.txt"]);
    // the summary, and the refusals
    let o = ok(&d, None, &["--diff", "--at", "#2", "a.aqr"]);
    assert!(stderr(&o).contains("1 added, 1 changed, 0 removed (#1 \"p1\" -> #2 \"p2\")"), "{}", stderr(&o));
    for args in [
        ["--diff", "--at", "#9", "a.aqr"].as_slice(),
        &["--from", "#1", "-t", "a.aqr"],
        &["--diff", "--history", "a.aqr"],
        &["--diff", "-t", "a.aqr"],
    ] {
        assert!(!aqr_at(&d, None, args).status.success(), "{args:?} succeeded");
    }
    ok(&d, Some(&day(5)), &["-q", "-j", "only", "-o", "j.aqr", "src"]);
    assert_eq!(stdout(&ok(&d, None, &["--diff", "-q", "--at", "#1", "j.aqr"])).lines().count(), 4);
    // as JSON, for a frontend
    let j: serde_json::Value =
        serde_json::from_str(stdout(&ok(&d, None, &["--diff", "--json", "--at", "#2", "a.aqr"])).trim()).unwrap();
    assert_eq!(j["archive"], "a.aqr");
    assert_eq!((j["added"].as_u64(), j["changed"].as_u64(), j["removed"].as_u64()), (Some(1), Some(1), Some(0)));
    assert_eq!(j["changes"][0], serde_json::json!({"change": "changed", "path": "a.txt", "dir": false}));
    assert_eq!(j["changes"][1]["change"], "added");
    let j: serde_json::Value =
        serde_json::from_str(stdout(&ok(&d, None, &["--diff", "--json", "--at", "@p5", "a.aqr"])).trim()).unwrap();
    assert_eq!(j["changes"][0], serde_json::json!({"change": "added", "path": "sub", "dir": true}));
    // the archive against paths, as -a takes them: nothing differs yet
    let against = |args: &[&str]| -> Vec<String> {
        let mut all = vec!["--diff", "-q"];
        all.extend_from_slice(args);
        stdout(&ok(&d, None, &all)).lines().map(str::to_owned).collect()
    };
    assert_eq!(against(&["a.aqr", "src"]), Vec::<String>::new());
    fs::write(src.join("a.txt"), words(9, 3_700)).unwrap();
    fs::write(src.join("e.txt"), words(8, 300)).unwrap();
    fs::remove_file(src.join("sub/c.txt")).unwrap();
    assert_eq!(against(&["a.aqr", "src"]), ["~ a.txt", "+ e.txt", "- sub/c.txt"]);
    // a point of it against them
    assert_eq!(against(&["--at", "#1", "a.aqr", "src"]), ["~ a.txt", "+ d.txt", "+ e.txt", "+ sub/"]);
    let o = ok(&d, None, &["--diff", "a.aqr", "src"]);
    assert!(stderr(&o).contains("only in src"), "{}", stderr(&o));
    for args in [
        ["--diff", "--from", "#1", "a.aqr", "src"].as_slice(),
        &["--diff", "a.aqr", "nowhere"],
        &["--diff", "--rm", "a.aqr", "src"],
    ] {
        assert!(!aqr_at(&d, None, args).status.success(), "{args:?} succeeded");
    }
    let o = aqr_at(&d, None, &["--diff", "plain.aqr"]);
    assert!(!o.status.success());
}

/// `--tag` names the state an archive is in, one made without `-j` too, and changes nothing else.
#[test]
fn naming_a_state() {
    let d = scratch("tag");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), words(1, 2_000)).unwrap();
    ok(&d, None, &["-q", "-o", "a.aqr", "src"]);
    let before = states_of(&d, "a.aqr");
    ok(&d, Some(&day(0)), &["-q", "--tag", "release {today}", "a.aqr"]);
    fs::write(src.join("b.txt"), words(2, 1_000)).unwrap();
    ok(&d, None, &["-q", "-a", "a.aqr", "src"]);
    let o = ok(&d, Some(&day(1)), &["--tag", "later", "a.aqr"]);
    assert!(stderr(&o).contains("journal point \"later\" added"), "{}", stderr(&o));
    let h = stdout(&ok(&d, None, &["--history", "a.aqr"]));
    assert!(h.lines().count() == 2 && h.contains("release 2026-11-01"), "{h}");
    assert_eq!(listed(&d, &["-t", "-q", "--at", "@release 2026-11-01", "a.aqr"]), ["a.txt"]);
    assert_eq!(listed(&d, &["-t", "-q", "--at", "#2", "a.aqr"]), ["a.txt", "b.txt"]);
    assert_eq!(before, vec!["a.txt".to_string()]);
    ok(&d, None, &["-q", "-K", "a.aqr"]);
    for args in [
        ["--tag", "x"].as_slice(),
        &["--tag", "x", "a.aqr", "src"],
        &["--tag", "x", "-t", "a.aqr"],
        &["--tag", "x", "nowhere.aqr"],
        &["--tag", "{what}", "a.aqr"],
    ] {
        assert!(!aqr_at(&d, None, args).status.success(), "{args:?} succeeded");
    }
}

fn states_of(dir: &Path, archive: &str) -> Vec<String> {
    listed(dir, &["-t", "-q", archive])
}

/// Runs of segments that only name a state again (a `-j` run where nothing changed, `--tag`) are
/// written as one segment with all their points, by `--compact --keep-since`.
#[test]
fn empty_segments_are_merged() {
    let d = scratch("merge");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), words(1, 3_000)).unwrap();
    ok(&d, Some(&day(0)), &["-q", "-j", "p1", "-o", "a.aqr", "src"]);
    // three runs that change nothing, and a tag
    for (n, label) in ["u1", "u2", "u3"].into_iter().enumerate() {
        ok(&d, Some(&day(1 + n as u64)), &["-q", "-a", "-j", label, "a.aqr", "src"]);
    }
    ok(&d, Some(&day(4)), &["-q", "--tag", "t4", "a.aqr"]);
    fs::write(src.join("b.txt"), words(2, 1_000)).unwrap();
    ok(&d, Some(&day(5)), &["-q", "-a", "-j", "p6", "a.aqr", "src"]);
    ok(&d, Some(&day(6)), &["-q", "-a", "-j", "u7", "a.aqr", "src"]);
    let before = states(&d, "a.aqr", 7);
    assert_eq!(before[0], before[4]);
    assert_eq!(before[5], before[6]);
    assert_eq!(before[5].len(), 2);
    let size = fs::metadata(d.join("a.aqr")).unwrap().len();

    // every point kept (from the first): the seven segments are two
    let o = ok(&d, None, &["--compact", "--keep-since", "#1", "a.aqr"]);
    assert!(stderr(&o).contains("7 -> 2 segments"), "{}", stderr(&o));
    let h = stdout(&ok(&d, None, &["--history", "a.aqr"]));
    let labels: Vec<&str> = h.lines().map(|l| l.rsplit("  ").next().unwrap()).collect();
    assert_eq!(labels, ["p1", "u1", "u2", "u3", "t4", "p6", "u7"], "{h}");
    assert_eq!(states(&d, "a.aqr", 7), before);
    ok(&d, None, &["-q", "-K", "a.aqr"]);
    assert!(fs::metadata(d.join("a.aqr")).unwrap().len() < size);
    // the times are the ones named, and the dates find them
    assert_eq!(listed(&d, &["-t", "-q", "--at", "2026-11-04", "a.aqr"]), ["a.txt"]);
    assert_eq!(listed(&d, &["-t", "-q", "--at", "@u7", "a.aqr"]), ["a.txt", "b.txt"]);
    // appended to after that: as before
    ok(&d, Some(&day(7)), &["-q", "--tag", "t8", "a.aqr"]);
    let h = stdout(&ok(&d, None, &["--history", "a.aqr"]));
    assert_eq!(h.lines().count(), 8, "{h}");
    ok(&d, None, &["-q", "-K", "a.aqr"]);
}
