//! Writing and appending archives at the edges (docs/edge-tests-todo.md, "Writing and appending").

mod common;
use common::{files, stderr, text};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    common::scratch("write", name)
}

fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

/// A directory `src` of `n` text files under `d`.
fn source(d: &Path, n: u64) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..n {
        fs::write(src.join(format!("t{i}.txt")), text(10 + i, 3000 + 700 * i as usize)).unwrap();
    }
    files(&src)
}

fn extracted(d: &Path, name: &str) -> Vec<(String, Vec<u8>)> {
    let out = format!("out-{name}");
    let _ = fs::remove_dir_all(d.join(&out));
    ok(d, &["-q", "-d", "-o", &out, name]);
    files(&d.join(&out))
}

/// write/1: an archive of mirror blocks (`-A`) is not appended to, journalled or edited in
/// place: what was written after its mirror blocks would go unmirrored. It is left as it is.
#[test]
fn mirrored_archive_not_appended() {
    let d = scratch("mirrored");
    source(&d, 5);
    ok(&d, &["-q", "-A", "-o", "m.aqr", "src"]);
    let before = fs::read(d.join("m.aqr")).unwrap();
    fs::write(d.join("src/new.txt"), text(99, 5000)).unwrap();
    let cases: [&[&str]; 5] = [
        &["-a", "m.aqr", "src"],
        &["-a", "-j", "x", "m.aqr", "src"],
        &["--tag", "x", "m.aqr"],
        &["--delete", "m.aqr", "t1.txt"],
        &["--compact", "m.aqr"],
    ];
    for args in cases {
        let o = aqr(&d, args);
        assert!(!o.status.success() && stderr(&o).contains("mirror blocks"), "{args:?}: {}", stderr(&o));
        assert_eq!(fs::read(d.join("m.aqr")).unwrap(), before, "{args:?}");
    }
    assert_eq!(extracted(&d, "m.aqr").len(), 5);
    // (an archive without them is: the check is not of every archive)
    ok(&d, &["-q", "-o", "p.aqr", "src"]);
    ok(&d, &["-q", "-a", "p.aqr", "src"]);
    fs::remove_dir_all(&d).unwrap();
}

/// The length of a stub to put before archive `a` so that its footer's first bytes (those a
/// commit makes a spacer block) cross a sector of the file, or none.
fn stub_for_crossing(a: &[u8], crossing: bool) -> usize {
    let at = aqr_archive::locate(a).unwrap();
    let (footer, _) = aqr_archive::footer_of(&a[at.clone()]).unwrap();
    let n = aqr_archive::kill(footer.len()).len();
    (0..512).find(|p| aqr_archive::crossing((p + at.start + footer.start) as u64, n) == crossing).unwrap()
}

/// write/2: a footer across a sector of the file is written over (the file cut at it, the new
/// footer after) rather than made a spacer block; behind a stub too. A file cut as a crash
/// between the two leaves it, whole up to the footer, is still appended to.
#[test]
fn footer_across_a_sector() {
    let d = scratch("sector");
    source(&d, 6);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    for crossing in [false, true] {
        let stub = vec![b'#'; stub_for_crossing(&a, crossing)];
        let name = format!("s{crossing}.aqr");
        fs::write(d.join(&name), [&stub[..], &a].concat()).unwrap();
        fs::write(d.join("src/new.txt"), text(99, 5000)).unwrap();
        let o = ok(&d, &["-v", "-a", &name, "src"]);
        assert_eq!(stderr(&o).contains("across a sector"), crossing, "{}", stderr(&o));
        let got = fs::read(d.join(&name)).unwrap();
        assert!(got.starts_with(&stub) && got.len() > a.len() + stub.len());
        assert_eq!(extracted(&d, &name), files(&d.join("src")));
        let t = ok(&d, &["-K", &name]);
        assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
        // and again, from the file as it is now
        fs::write(d.join("src/new.txt"), text(98, 7000)).unwrap();
        ok(&d, &["-q", "-a", &name, "src"]);
        assert_eq!(extracted(&d, &name), files(&d.join("src")));
        fs::remove_file(d.join("src/new.txt")).unwrap();
    }
    // the crash: the file cut at its footer (as `set_len` left it), before the new one is written
    let at = aqr_archive::locate(&a).unwrap();
    let (footer, _) = aqr_archive::footer_of(&a[at.clone()]).unwrap();
    fs::write(d.join("cut.aqr"), &a[..footer.start]).unwrap();
    let o = ok(&d, &["-d", "-o", "cut-out", "cut.aqr"]);
    assert!(stderr(&o).contains("footer"), "{}", stderr(&o));
    assert_eq!(files(&d.join("cut-out")), source(&d, 6));
    // (and to be appended to: told what it is, the file left alone)
    let o = aqr(&d, &["-a", "cut.aqr", "src"]);
    assert!(!o.status.success() && stderr(&o).contains("cut.aqr"), "{}", stderr(&o));
    assert_eq!(fs::read(d.join("cut.aqr")).unwrap(), &a[..footer.start]);
    fs::remove_dir_all(&d).unwrap();
}

/// write/3: an archive another process holds the lock of is not appended to nor edited, and is
/// left as it is.
#[cfg(unix)]
#[test]
fn locked_archive() {
    let d = scratch("locked");
    source(&d, 4);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let before = fs::read(d.join("a.aqr")).unwrap();
    fs::write(d.join("src/new.txt"), text(5, 4000)).unwrap();
    let held = fs::OpenOptions::new().read(true).write(true).open(d.join("a.aqr")).unwrap();
    held.lock().unwrap();
    let cases: [&[&str]; 4] =
        [&["-a", "a.aqr", "src"], &["--delete", "a.aqr", "t1.txt"], &["--compact", "a.aqr"], &["--tag", "x", "a.aqr"]];
    for args in cases {
        let o = aqr(&d, args);
        assert!(!o.status.success() && stderr(&o).contains("being written by another"), "{args:?}: {}", stderr(&o));
        assert_eq!(fs::read(d.join("a.aqr")).unwrap(), before, "{args:?}");
    }
    drop(held);
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    fs::remove_dir_all(&d).unwrap();
}

/// write/3: an append with `--atomic` stopped by a signal leaves the archive as it was, or as a
/// whole commit of it.
#[cfg(unix)]
#[test]
fn atomic_append_stopped() {
    let d = scratch("atomic-append");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..300 {
        fs::write(src.join(format!("f{i:03}.txt")), text(i, 40_000)).unwrap();
    }
    let half = d.join("half");
    fs::create_dir_all(&half).unwrap();
    for i in 0..50 {
        let n = format!("f{i:03}.txt");
        fs::copy(src.join(&n), half.join(&n)).unwrap();
        let m = fs::metadata(src.join(&n)).unwrap().modified().unwrap();
        fs::OpenOptions::new().write(true).open(half.join(&n)).unwrap().set_modified(m).unwrap();
    }
    ok(&d, &["-q", "-o", "b.aqr", "half"]);
    let before = fs::read(d.join("b.aqr")).unwrap();
    let start = std::time::Instant::now();
    let (o, running) = signalled(
        &d,
        &["--atomic", "-a", "-T", "1", "-B", "100K", "-S", "1", "b.aqr", "src"],
        &|| start.elapsed() > std::time::Duration::from_millis(500),
        &["TERM"],
    );
    assert!(running, "done before it was stopped");
    assert!(!o.status.success(), "{}", stderr(&o));
    // (killed by the signal where it is read before anything is written, else exit code 130:
    // either way the archive is as it was, no commit of it half done)
    assert!(matches!(o.status.code(), None | Some(130)), "{:?} {}", o.status, stderr(&o));
    assert_eq!(fs::read(d.join("b.aqr")).unwrap(), before);
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
    // (and appended to whole after)
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    assert_eq!(extracted(&d, "b.aqr"), files(&src));
    fs::remove_dir_all(&d).unwrap();
}

/// Of `args` in `dir`, sent `signals` once `when` holds (or 5 seconds have gone by): its outcome,
/// and whether it was still running when they were sent.
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

/// write/3: an append cancelled by a frontend (`--ipc`'s cancel, or its standard input closed) is
/// stopped, and the archive appended to is not removed: it holds what it did before.
#[cfg(unix)]
#[test]
fn ipc_append_cancelled() {
    use std::io::{BufRead, BufReader, Write};
    for by_message in [true, false] {
        let d = scratch(if by_message { "ipc-cancel" } else { "ipc-eof" });
        let want = source(&d, 3);
        ok(&d, &["-q", "-o", "b.aqr", "src"]);
        let before = fs::read(d.join("b.aqr")).unwrap();
        let mut x = 1u64;
        let data: Vec<u8> = (0..32 << 20)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                b"abcdefgh \n"[(x >> 60) as usize % 10]
            })
            .collect();
        fs::write(d.join("src/big"), data).unwrap();
        let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
            .current_dir(&d)
            .args(["--ipc", "-a", "b.aqr", "src"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let i = c.stdin.as_mut().unwrap();
        writeln!(i, r#"{{"hello":1,"answers":[]}}"#).unwrap();
        i.flush().unwrap();
        let mut lines = BufReader::new(c.stdout.take().unwrap()).lines().map(|l| l.unwrap());
        assert!(lines.next().unwrap().contains("hello"));
        assert!(lines.next().unwrap().contains("begin"));
        if by_message {
            let i = c.stdin.as_mut().unwrap();
            writeln!(i, r#"{{"control":"cancel"}}"#).unwrap();
            i.flush().unwrap();
        } else {
            drop(c.stdin.take());
        }
        let rest: Vec<String> = lines.filter(|l| !l.contains("progress")).collect();
        assert_eq!(c.wait().unwrap().code(), Some(130), "{rest:?}");
        assert!(rest.iter().any(|l| l.contains("stopped")), "{rest:?}");
        // (as it was, or whole with what was committed of the append: never gone nor torn)
        let after = fs::read(d.join("b.aqr")).expect("the archive appended to removed");
        assert!(after.len() >= before.len());
        let t = ok(&d, &["-K", "b.aqr"]);
        assert!(!stderr(&t).contains("damaged"), "{}", stderr(&t));
        let got = extracted(&d, "b.aqr");
        assert!(want.iter().all(|f| got.contains(f)));
        fs::remove_dir_all(&d).unwrap();
    }
}
