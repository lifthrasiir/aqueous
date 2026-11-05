//! Archives located in files (other bytes before or after them), stopped as they are written,
//! and appended to (`-a`; spec, "Appending to an archive").

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-append-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn text(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
    let mut t = Vec::new();
    while t.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        t.extend_from_slice(format!("{} {} ", words[(x >> 61) as usize], (x >> 40) % 1000).as_bytes());
    }
    t.truncate(n);
    t
}

/// The files under `d`, by path from it.
fn files(d: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![(d.to_owned(), String::new())];
    while let Some((dir, at)) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = format!("{at}{}", e.file_name().to_string_lossy());
            if e.file_type().unwrap().is_dir() {
                stack.push((e.path(), format!("{name}/")));
            } else {
                out.push((name, fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// A directory `src` of `n` text files under `d`.
fn source(d: &Path, n: u64) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    for i in 0..n {
        let at = if i % 3 == 0 { format!("sub/t{i}.txt") } else { format!("t{i}.txt") };
        fs::write(src.join(at), text(10 + i, 3000 + 700 * i as usize)).unwrap();
    }
    files(&src)
}

/// An archive after a stub and before bytes added to it is extracted, those bytes told.
#[test]
fn located_among_other_bytes() {
    let d = scratch("located");
    let want = source(&d, 4);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    fs::write(d.join("lead.aqr"), [b"#!stub\n".as_slice(), &a].concat()).unwrap();
    fs::write(d.join("trail.aqr"), [&a, b"added".as_slice()].concat()).unwrap();
    for (name, told) in
        [("lead.aqr", "7 bytes before the archive left out"), ("trail.aqr", "5 bytes after the archive left out")]
    {
        let o = ok(&d, &["-d", "-o", &format!("out-{name}"), name]);
        assert!(stderr(&o).contains(told), "{}", stderr(&o));
        assert_eq!(files(&d.join(format!("out-{name}"))), want);
    }
    fs::remove_dir_all(&d).unwrap();
}
