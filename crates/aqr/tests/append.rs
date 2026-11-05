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

/// The files of archive `name` in `d`, extracted.
fn extracted(d: &Path, name: &str) -> Vec<(String, Vec<u8>)> {
    let to = d.join("x");
    let _ = fs::remove_dir_all(&to);
    ok(d, &["-d", "-o", "x", name]);
    let got = files(&to);
    fs::remove_dir_all(&to).unwrap();
    got
}

/// A file's modification time moved, as an edit would.
fn touched(path: &Path, seconds: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + seconds))
        .unwrap();
}

/// Inputs appended to an archive (`-a`): what is new or changed added, what is the same not
/// written again; with `--delete`, what is gone taken away; nothing to append, nothing written.
#[test]
fn appended() {
    let d = scratch("appended");
    source(&d, 6);
    let src = d.join("src");
    ok(&d, &["-q", "-o", "b.aqr", "src"]);
    let before = fs::read(d.join("b.aqr")).unwrap();
    // nothing changed: nothing written
    let o = ok(&d, &["-a", "b.aqr", "src"]);
    assert!(stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    assert_eq!(fs::read(d.join("b.aqr")).unwrap(), before);
    // a file changed, one added, in a new directory too
    fs::write(src.join("t1.txt"), text(77, 5000)).unwrap();
    touched(&src.join("t1.txt"), 5);
    fs::create_dir_all(src.join("new")).unwrap();
    fs::write(src.join("new/n.txt"), text(78, 9000)).unwrap();
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    let after = fs::read(d.join("b.aqr")).unwrap();
    assert!(after.len() > before.len() && after.len() < before.len() * 2, "{} {}", before.len(), after.len());
    assert_eq!(extracted(&d, "b.aqr"), files(&src));
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
    // (only appended to: the archive before, but for its footer made a spacer block)
    let differ = (0..before.len()).filter(|&k| before[k] != after[k]).count();
    assert!(differ > 0 && differ < 30, "{differ}");
    // the commit written whole but its footer before not made a spacer: read as the archive
    // before, then made by the next append
    fs::write(d.join("b.aqr"), [&before[..], &after[before.len()..]].concat()).unwrap();
    assert_eq!(extracted(&d, "b.aqr").len(), 6);
    let o = ok(&d, &["-a", "b.aqr", "src"]);
    assert!(stderr(&o).contains("was whole: committed") && stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    assert_eq!(fs::read(d.join("b.aqr")).unwrap(), after);
    // a file taken away: kept in the archive, but with --delete
    fs::remove_file(src.join("t2.txt")).unwrap();
    fs::remove_dir_all(src.join("new")).unwrap();
    fs::write(src.join("t4.txt"), b"short").unwrap();
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    let got = extracted(&d, "b.aqr");
    assert!(got.iter().any(|f| f.0 == "t2.txt") && got.iter().any(|f| f.0 == "new/n.txt"));
    assert!(got.contains(&("t4.txt".into(), b"short".to_vec())));
    ok(&d, &["-q", "-a", "--delete", "b.aqr", "src"]);
    assert_eq!(extracted(&d, "b.aqr"), files(&src));
    // not an encrypted archive, nor -P
    assert!(!aqr(&d, &["-a", "-P", "b.aqr", "src"]).status.success());
    fs::remove_dir_all(&d).unwrap();
}

/// An archive appended to whose append was not committed (bytes after it, as a process killed
/// leaves): the bytes cut off, or the commit made where it was written whole; other bytes after
/// it not taken away.
#[test]
fn appended_after_append_not_committed() {
    let d = scratch("tail");
    source(&d, 4);
    ok(&d, &["-q", "-o", "b.aqr", "src"]);
    let a = fs::read(d.join("b.aqr")).unwrap();
    // (a block begun: a spacer block's head)
    let pad = if a.len() % 2 == 1 { &[0u8][..] } else { &[] };
    fs::write(d.join("b.aqr"), [&a, pad, &[0x03, 0xff, 0xff]].concat()).unwrap();
    fs::write(d.join("src/t9.txt"), text(9, 4000)).unwrap();
    let o = ok(&d, &["-a", "b.aqr", "src"]);
    assert!(stderr(&o).contains("an append not committed cut off"), "{}", stderr(&o));
    assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
    // bytes not of an append
    let a = fs::read(d.join("b.aqr")).unwrap();
    fs::write(d.join("b.aqr"), [&a, &[0xffu8; 40][..]].concat()).unwrap();
    fs::write(d.join("src/t10.txt"), text(10, 4000)).unwrap();
    let o = aqr(&d, &["-a", "b.aqr", "src"]);
    assert!(!o.status.success() && stderr(&o).contains("not of an append"), "{}", stderr(&o));
    fs::remove_dir_all(&d).unwrap();
}

/// An append stopped (as Ctrl-C does) ends what it wrote as a commit; appending again adds the
/// rest.
#[cfg(unix)]
#[test]
fn append_stopped_and_resumed() {
    let d = scratch("resumed");
    many(&d);
    let src = d.join("src");
    // (half of it first)
    let half = d.join("half");
    fs::create_dir_all(&half).unwrap();
    for e in fs::read_dir(&src).unwrap().take(100) {
        let e = e.unwrap();
        fs::copy(e.path(), half.join(e.file_name())).unwrap();
        let m = fs::metadata(e.path()).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(half.join(e.file_name()))
            .unwrap()
            .set_modified(m.modified().unwrap())
            .unwrap();
    }
    ok(&d, &["-q", "-o", "b.aqr", "half"]);
    let out = d.join("b.aqr");
    let len = fs::metadata(&out).unwrap().len();
    let args: Vec<&str> = ["-a"].into_iter().chain(SLOW).chain(["src"]).collect();
    let (o, running) = signalled(&d, &args, &|| fs::metadata(&out).unwrap().len() > len + 200_000, &["TERM"]);
    assert!(running, "done before it was stopped");
    assert_eq!(o.status.code(), Some(130), "{}", stderr(&o));
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(stderr(&t).contains("incomplete"), "{}", stderr(&t));
    let got = extracted(&d, "b.aqr");
    assert!(got.len() > 100 && got.len() < 300, "{}", got.len());
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    assert_eq!(extracted(&d, "b.aqr"), files(&src));
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
    fs::remove_dir_all(&d).unwrap();
}
