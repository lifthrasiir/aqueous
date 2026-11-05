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

/// `aqr` given `args` in `dir`, sent `signals` (one after another) once `when` holds (or 5
/// seconds have gone by): its outcome, and whether it was still running when they were sent.
#[cfg(unix)]
fn signalled(dir: &Path, args: &[&str], when: &dyn Fn() -> bool, signals: &[&str]) -> (Output, bool) {
    let c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !when() && start.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let pid = c.id().to_string();
    let mut running = true;
    for s in signals {
        running &=
            Command::new("kill").args([&format!("-{s}"), &pid]).stderr(Stdio::null()).status().unwrap().success();
    }
    (c.wait_with_output().unwrap(), running)
}

/// Many small files, archived a segment a batch, slowly enough to be stopped.
#[cfg(unix)]
fn many(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..300 {
        fs::write(src.join(format!("f{i:03}.txt")), text(i, 40_000)).unwrap();
    }
    files(&src)
}

#[cfg(unix)]
const SLOW: [&str; 8] = ["-T", "1", "-B", "100K", "-S", "1", "-o", "b.aqr"];

/// An archive stopped (a signal, as Ctrl-C is) once a segment of it is committed: ended as it
/// is, of the files written, telling what it left out; the run failing as a shell tells
/// Ctrl-C. Stopped twice, at once: the archive of its last commit.
#[cfg(unix)]
#[test]
fn stopped() {
    let d = scratch("stopped");
    let all = many(&d);
    let out = d.join("b.aqr");
    for signals in [&["TERM"][..], &["TERM", "TERM"]] {
        let _ = fs::remove_file(&out);
        let args: Vec<&str> = SLOW.iter().copied().chain(["src"]).collect();
        let (o, running) = signalled(&d, &args, &|| out.exists(), signals);
        assert!(running, "done before it was stopped");
        if signals.len() == 1 {
            assert_eq!(o.status.code(), Some(130), "{}", stderr(&o));
            assert!(stderr(&o).contains("aqr: stopped: b.aqr holds"), "{}", stderr(&o));
        }
        let t = ok(&d, &["-K", "b.aqr"]);
        assert!(stderr(&t).contains("incomplete, its writing stopped"), "{}", stderr(&t));
        ok(&d, &["-d", "-f", "-o", "x", "b.aqr"]);
        let got = files(&d.join("x"));
        fs::remove_dir_all(d.join("x")).unwrap();
        assert!(!got.is_empty() && got.len() < all.len(), "{}", got.len());
        assert!(got.iter().all(|f| all.contains(f)));
        let parts =
            fs::read_dir(&d).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".part"));
        assert_eq!(parts.count(), 0);
    }
    // with --atomic, nothing
    let _ = fs::remove_file(&out);
    let args: Vec<&str> = ["--atomic"].into_iter().chain(SLOW).chain(["src"]).collect();
    let start = std::time::Instant::now();
    let (o, running) = signalled(&d, &args, &|| start.elapsed() > std::time::Duration::from_millis(500), &["TERM"]);
    assert!(running && !o.status.success());
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("b.aqr")));
    fs::remove_dir_all(&d).unwrap();
}
