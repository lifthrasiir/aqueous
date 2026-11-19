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

fn aqr_in(d: &std::path::Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// The runs of compressing, testing and decompressing (a tree, and a single file), each told
/// in the JSON and the page, and what stood out on standard error.
#[test]
fn runs_are_told() {
    let d = scratch("runs");
    let text: Vec<u8> = (0..20000).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
    fs::create_dir_all(d.join("t/sub")).unwrap();
    fs::write(d.join("t/a.txt"), &text).unwrap();
    fs::write(d.join("t/sub/b.txt"), &text[..5000]).unwrap();
    fs::write(d.join("single.txt"), &text).unwrap();
    let run = |args: &[&str], file: &str| {
        let (ok, err) = aqr_in(&d, args);
        assert!(ok, "{args:?}: {err}");
        assert!(err.contains("stats: "), "{err}");
        assert!(err.lines().any(|l| l.starts_with("stats written to ")), "{err}");
        let v: serde_json::Value = serde_json::from_slice(&fs::read(d.join(file)).unwrap()).unwrap();
        assert_eq!(v["state"], "finished");
        v["runs"][0].clone()
    };
    let r = run(&["-q", "--stats", "--stats=c.json", "--stats=c.html", "-o", "t.aqr", "t"], "c.json");
    assert_eq!(r["mode"], "compress");
    let paths: Vec<&str> = r["contents"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
    assert!(paths.iter().any(|p| p.ends_with("a.txt")) && paths.iter().any(|p| p.ends_with("b.txt")), "{paths:?}");
    assert!(fs::read_to_string(d.join("c.html")).unwrap().contains("\"mode\":\"compress\""));

    let r = run(&["-Kq", "--stats", "--stats=k.json", "t.aqr"], "k.json");
    assert_eq!(r["mode"], "test");
    let r = run(&["-dq", "--stats", "--stats=d.json", "-o", "out", "t.aqr"], "d.json");
    assert_eq!(r["mode"], "decompress");
    assert_eq!(r["written"].as_u64().unwrap(), (text.len() + 5000) as u64);
    let lens: Vec<u64> = r["contents"].as_array().unwrap().iter().map(|c| c["len"].as_u64().unwrap()).collect();
    assert!(lens.contains(&(text.len() as u64)) && lens.contains(&5000), "{lens:?}");
    assert_eq!(fs::read(d.join("out/a.txt")).unwrap(), text);

    // a single file: one content, of the length decoded
    run(&["-q", "--stats", "--stats=s.json", "single.txt"], "s.json");
    let r = run(&["-dq", "--stats", "--stats=sd.json", "-o", "single.out", "single.txt.aqr"], "sd.json");
    assert_eq!(r["contents"][0]["len"].as_u64().unwrap(), text.len() as u64);
    assert_eq!(fs::read(d.join("single.out")).unwrap(), text);
    fs::remove_dir_all(&d).unwrap();
}
