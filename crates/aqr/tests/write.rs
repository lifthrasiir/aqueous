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
    let out = format!("out-{}", name.replace('/', "_"));
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
/// left as it is. (Windows' lock is mandatory, so the archive is compared once it is let go.)
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
    }
    drop(held);
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), before);
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

/// Many files, slowly archived (the last, `f119.txt`, read after the archive's first commit).
#[cfg(unix)]
fn slow_inputs(d: &Path) {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..120 {
        fs::write(src.join(format!("f{i:03}.txt")), text(i, 40_000)).unwrap();
    }
}

/// `--rm` archiving, the last input changed once the archive has its first commit (`what`): the
/// archive is of what was read; a file that changed is kept, told so, and the run fails; one that
/// is gone or unreadable stops the archive where it is (not removing anything), which `-a`
/// completes once it is mended.
#[cfg(unix)]
#[test]
fn inputs_changing_while_archived() {
    use std::os::unix::fs::PermissionsExt;
    for what in ["cut", "grow", "same-size", "delete", "unreadable"] {
        let d = scratch(&format!("changing-{what}"));
        slow_inputs(&d);
        let before = files(&d.join("src"));
        let out = d.join("b.aqr");
        let last = d.join("src/f119.txt");
        let (o, _) = signalled(
            &d,
            &["--rm", "-T", "1", "-B", "100K", "-S", "1", "-o", "b.aqr", "src"],
            &|| {
                if !out.exists() {
                    return false;
                }
                match what {
                    "cut" => fs::write(&last, text(299, 100)).unwrap(),
                    "grow" => fs::write(&last, text(299, 90_000)).unwrap(),
                    "same-size" => fs::write(&last, text(298, 40_000)).unwrap(),
                    "delete" => fs::remove_file(&last).unwrap(),
                    _ => fs::set_permissions(&last, fs::Permissions::from_mode(0)).unwrap(),
                }
                true
            },
            &[],
        );
        let e = stderr(&o);
        assert!(!o.status.success(), "{what}: {e}");
        let t = ok(&d, &["-K", "b.aqr"]);
        let listed = fs::read_dir(d.join("src")).map(|r| r.count()).unwrap_or(0);
        match what {
            "cut" | "grow" | "same-size" => {
                assert!(e.contains("kept src/f119.txt (changed since it was read)"), "{what}: {e}");
                assert!(!stderr(&t).contains("incomplete"), "{what}: {}", stderr(&t));
                // (what it holds is what it read, the file as it was then or as it is now)
                let got = extracted(&d, "b.aqr");
                assert_eq!(got.len(), 120);
                let now = fs::read(&last).unwrap();
                let held = &got.iter().find(|f| f.0 == "f119.txt").unwrap().1;
                assert!(*held == now || *held == before[119].1, "{what}");
                assert_eq!(listed, 1, "{what}: only the changed file stays");
            }
            _ => {
                assert!(e.contains("f119.txt"), "{what}: {e}");
                assert!(stderr(&t).contains("incomplete"), "{what}: {}", stderr(&t));
                // (nothing removed: the archive is not whole)
                assert_eq!(listed, if what == "delete" { 119 } else { 120 }, "{what}");
                let _ = fs::set_permissions(&last, fs::Permissions::from_mode(0o644));
                if what == "delete" {
                    fs::write(&last, &before[119].1).unwrap();
                }
                ok(&d, &["-q", "-a", "b.aqr", "src"]);
                assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")), "{what}");
            }
        }
        fs::remove_dir_all(&d).unwrap();
    }
}

/// `aqr args` in `dir` with files limited to `kib` KiB (what a full disk does to a write: the
/// signal ignored, the write failing).
#[cfg(unix)]
fn limited(dir: &Path, kib: u32, args: &[&str]) -> Output {
    Command::new("sh")
        .current_dir(dir)
        .arg("-c")
        .arg(format!("trap '' XFSZ; ulimit -f {kib}; exec \"$0\" \"$@\""))
        .arg(env!("CARGO_BIN_EXE_aqr"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// write/5: a write failing for want of room (as a full disk does) fails the run, tells which
/// file, and leaves no archive passed off as whole: nothing where it was being made, the
/// archive as it was where appended to (what was written after it cut off by the next append).
#[cfg(unix)]
#[test]
fn disk_full() {
    let d = scratch("full");
    source(&d, 5);
    for args in [&["-o", "full.aqr"][..], &["--atomic", "-o", "full.aqr"], &["-o", "full.zip"], &["-o", "full.tar.gz"]]
    {
        let args: Vec<&str> = args.iter().copied().chain(["src"]).collect();
        let o = limited(&d, 3, &args);
        let e = stderr(&o);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {e}");
        assert!(e.contains("full.") && e.contains("File too large"), "{args:?}: {e}");
        assert!(fs::read_dir(&d).unwrap().all(|e| e.unwrap().file_name() == "src"), "{args:?}");
    }
    // appended to
    ok(&d, &["-q", "-o", "b.aqr", "src"]);
    let before = fs::read(d.join("b.aqr")).unwrap();
    for i in 10..16 {
        fs::write(d.join(format!("src/n{i}.txt")), text(i, 6000)).unwrap();
    }
    let o = limited(&d, 9, &["-a", "b.aqr", "src"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("File too large"), "{}", stderr(&o));
    let now = fs::read(d.join("b.aqr")).unwrap();
    assert!(now.starts_with(&before));
    assert_eq!(extracted(&d, "b.aqr").len(), 5);
    let o = ok(&d, &["-a", "b.aqr", "src"]);
    assert!(stderr(&o).contains("an append not committed cut off"), "{}", stderr(&o));
    assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
    fs::remove_dir_all(&d).unwrap();
}

/// write/5: an archive made inside the tree it archives is not among its files while it is made;
/// replaced (`-f`), it is an input like any file, read as it was; appended to, it is not
/// archived into itself (skipped, as Windows' lock on it would not let it be read), the entry of
/// it before left as it was.
#[test]
fn archive_in_its_input_tree() {
    let d = scratch("in-tree");
    source(&d, 5);
    let names = |g: &[(String, Vec<u8>)]| g.iter().map(|f| f.0.clone()).collect::<Vec<_>>();
    let plain = files(&d.join("src"));
    ok(&d, &["-q", "-o", "src/a.aqr", "src"]);
    let first = fs::read(d.join("src/a.aqr")).unwrap();
    assert_eq!(extracted(&d, "src/a.aqr"), plain);
    ok(&d, &["-q", "-f", "-o", "src/a.aqr", "src"]);
    let second = fs::read(d.join("src/a.aqr")).unwrap();
    let got = extracted(&d, "src/a.aqr");
    assert_eq!(names(&got), names(&files(&d.join("src"))));
    assert_eq!(got.iter().find(|f| f.0 == "a.aqr").unwrap().1, first);
    let o = ok(&d, &["-a", "src/a.aqr", "src"]);
    assert!(stderr(&o).contains("(the archive appended to)"), "{}", stderr(&o));
    let got = extracted(&d, "src/a.aqr");
    assert_eq!(got.iter().find(|f| f.0 == "a.aqr").unwrap().1, first);
    assert!(fs::read(d.join("src/a.aqr")).unwrap().starts_with(&second[..second.len() - 16]));
    let t = ok(&d, &["-K", "src/a.aqr"]);
    assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
    fs::remove_dir_all(&d).unwrap();
}

/// write/6: a hundred appends in a row: each commit's footer made a spacer block, every state
/// a whole archive, the file growing by what was added (and no more than a little over it).
#[test]
fn hundred_appends() {
    let d = scratch("hundred");
    source(&d, 1);
    ok(&d, &["-q", "-o", "b.aqr", "src"]);
    let mut sizes = vec![fs::metadata(d.join("b.aqr")).unwrap().len()];
    for i in 0..100 {
        fs::write(d.join(format!("src/a{i:03}.txt")), text(1000 + i, 700)).unwrap();
        ok(&d, &["-q", "-a", "b.aqr", "src"]);
        sizes.push(fs::metadata(d.join("b.aqr")).unwrap().len());
        if i % 25 == 0 {
            assert_eq!(extracted(&d, "b.aqr").len(), 2 + i as usize, "after {i}");
        }
    }
    assert!(sizes.windows(2).all(|w| w[1] > w[0]));
    // (each of 700 bytes of text, so small a commit has its own overhead: it stays bounded)
    let per = (sizes[100] - sizes[0]) / 100;
    assert!(per < 700, "{per} bytes an append");
    assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
    ok(&d, &["-q", "--compact", "b.aqr"]);
    assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
    assert!(fs::metadata(d.join("b.aqr")).unwrap().len() < sizes[100]);
    fs::remove_dir_all(&d).unwrap();
}

/// write/6: inputs of no files (an empty directory) make an archive of none, which is appended
/// to; an archive of a single file is not appended to nor edited; the only file of an archive
/// deleted leaves an archive of none (and a path given twice is refused).
#[test]
fn archives_of_no_files_and_of_one() {
    let d = scratch("zero");
    source(&d, 3);
    fs::create_dir_all(d.join("empty/sub")).unwrap();
    let o = ok(&d, &["-o", "e.aqr", "empty"]);
    assert!(stderr(&o).contains("0 files"), "{}", stderr(&o));
    assert_eq!(extracted(&d, "e.aqr"), []);
    assert!(stderr(&ok(&d, &["-K", "e.aqr"])).contains("0 files"));
    let o = ok(&d, &["-a", "e.aqr", "empty"]);
    assert!(stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    ok(&d, &["-q", "-a", "e.aqr", "src"]);
    assert_eq!(extracted(&d, "e.aqr"), files(&d.join("src")));
    // -a with no input
    let o = aqr(&d, &["-a", "e.aqr"]);
    assert!(!o.status.success() && stderr(&o).contains("no inputs"), "{}", stderr(&o));
    // an archive of a single file
    ok(&d, &["-q", "-o", "f.aqr", "src/t0.txt"]);
    let before = fs::read(d.join("f.aqr")).unwrap();
    for args in [&["-a", "f.aqr", "src"][..], &["--delete", "f.aqr", "t0.txt"], &["--tag", "x", "f.aqr"]] {
        let o = aqr(&d, args);
        assert!(!o.status.success() && stderr(&o).contains("single file"), "{args:?}: {}", stderr(&o));
        assert_eq!(fs::read(d.join("f.aqr")).unwrap(), before);
    }
    // the only file of an archive deleted
    ok(&d, &["-q", "-o", "o.aqr", "src/t0.txt", "src/t1.txt"]);
    let o = aqr(&d, &["--delete", "o.aqr", "src/t0.txt", "src/t0.txt"]);
    assert!(!o.status.success() && stderr(&o).contains("given twice"), "{}", stderr(&o));
    ok(&d, &["-q", "--delete", "o.aqr", "src/t0.txt"]);
    assert_eq!(extracted(&d, "o.aqr").iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["src/t1.txt"]);
    ok(&d, &["-q", "--delete", "o.aqr", "src/t1.txt"]);
    assert_eq!(extracted(&d, "o.aqr"), []);
    ok(&d, &["-q", "--compact", "o.aqr"]);
    assert_eq!(extracted(&d, "o.aqr"), []);
    let o = aqr(&d, &["--delete", "o.aqr", "src/t1.txt"]);
    assert!(!o.status.success() && stderr(&o).contains("no src/t1.txt in it"), "{}", stderr(&o));
    ok(&d, &["-q", "-a", "o.aqr", "src"]);
    assert_eq!(extracted(&d, "o.aqr"), files(&d.join("src")));
    fs::remove_dir_all(&d).unwrap();
}

/// write/6: `-a` on an archive cut short or damaged ends in a clear error (or appends), never a
/// panic; where it fails the file is as it was.
#[test]
fn append_to_truncated_and_torn() {
    let d = scratch("torn");
    source(&d, 4);
    ok(&d, &["-q", "-o", "t.aqr", "src"]);
    let a = fs::read(d.join("t.aqr")).unwrap();
    fs::write(d.join("src/new.txt"), text(7, 3000)).unwrap();
    let mut tried: Vec<(String, Vec<u8>)> =
        [0, 5, 20, a.len() / 2, a.len() - 30, a.len() - 12, a.len() - 2, a.len() - 1]
            .map(|n| (format!("cut {n}"), a[..n].to_vec()))
            .into();
    for at in (0..a.len()).step_by(a.len() / 40) {
        let mut b = a.clone();
        b[at] ^= 0x55;
        tried.push((format!("flip {at}"), b));
    }
    let mut appended = 0;
    for (what, bytes) in tried {
        fs::write(d.join("c.aqr"), &bytes).unwrap();
        let o = aqr(&d, &["-a", "c.aqr", "src"]);
        let e = stderr(&o);
        assert!(matches!(o.status.code(), Some(0 | 1)) && !e.contains("panicked"), "{what}: {:?} {e}", o.status);
        if o.status.success() {
            appended += 1;
        } else {
            assert!(e.starts_with("aqr: c.aqr") || e.contains("c.aqr"), "{what}: {e}");
            assert_eq!(fs::read(d.join("c.aqr")).unwrap(), bytes, "{what}: not left as it was");
        }
    }
    // (what a damaged data block holds is found as it is read, not as it is appended to)
    assert!(appended > 0);
    fs::remove_dir_all(&d).unwrap();
}

/// write/7: more entries than a 16-bit count holds (65535 and a few over): written, listed and
/// extracted whole.
#[test]
fn over_65535_files() {
    let d = scratch("many");
    let src = d.join("src");
    for i in 0..660 {
        let sub = src.join(format!("d{i:03}"));
        fs::create_dir_all(&sub).unwrap();
        for j in 0..100 {
            fs::write(sub.join(format!("f{j:03}")), b"").unwrap();
        }
    }
    let o = ok(&d, &["-o", "m.aqr", "src"]);
    assert!(stderr(&o).contains("66000 files"), "{}", stderr(&o));
    let o = ok(&d, &["-t", "-q", "m.aqr"]);
    assert_eq!(o.stdout.iter().filter(|&&b| b == b'\n').count(), 66000 + 660, "{}", stderr(&o));
    let o = ok(&d, &["-d", "-o", "x", "m.aqr"]);
    assert!(stderr(&o).contains("66000 files"), "{}", stderr(&o));
    let (a, b) = (fs::read_dir(d.join("x")).unwrap().count(), fs::read_dir(&src).unwrap().count());
    assert_eq!((a, b), (660, 660));
    let last = fs::metadata(d.join("x/d659/f099")).unwrap();
    assert!(last.is_file() && last.len() == 0);
    // (appended to, one more)
    fs::write(src.join("d000/new"), b"n").unwrap();
    let o = ok(&d, &["-a", "m.aqr", "src"]);
    assert!(stderr(&o).contains("1 file"), "{}", stderr(&o));
    ok(&d, &["-q", "-K", "m.aqr"]);
    fs::remove_dir_all(&d).unwrap();
}

/// The entries under `d`: path and kind, by bytes, with the contents of files.
#[cfg(unix)]
fn walk(d: &Path) -> Vec<(Vec<u8>, Vec<u8>)> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = Vec::new();
    let mut stack = vec![(d.to_owned(), Vec::new())];
    while let Some((dir, at)) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = [&at[..], e.file_name().as_bytes()].concat();
            if e.file_type().unwrap().is_dir() {
                out.push((name.clone(), b"/".to_vec()));
                stack.push((e.path(), [&name[..], b"/"].concat()));
            } else {
                out.push((name, fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// write/7: names a Unix file system takes and other systems do not (a backslash, a colon, a
/// trailing dot or space, a device's name, bytes that are not UTF-8 or are a surrogate's, of
/// 255 bytes, a path of 900 bytes), and the times of files at extremes (before 1970, far on,
/// of 999999999 nanoseconds): archived by one thread and by eight to the same bytes, extracted
/// as they were. What the file system will not take is left out of the sources.
#[cfg(unix)]
#[test]
fn names_and_times_roundtrip() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::time::{Duration, SystemTime};
    let d = scratch("names");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    let long = "l".repeat(255);
    let names: Vec<Vec<u8>> = vec![
        b"back\\slash".to_vec(),
        b"a\\b\\c.txt".to_vec(),
        b"co:lon".to_vec(),
        b"trailing.".to_vec(),
        b"trailing ".to_vec(),
        b"CON".to_vec(),
        b"nul.txt".to_vec(),
        b"-dash".to_vec(),
        b"\xff\xfe-bytes".to_vec(),
        b"surrogate-\xed\xa0\x80".to_vec(),
        "emoji-\u{1F600}".into(),
        "\u{e9}-composed".into(),
        long.clone().into_bytes(),
        b"d\xffir/inner".to_vec(),
        format!("{long}/{long}").into_bytes(),
    ];
    let mut made = 0;
    for (i, n) in names.iter().enumerate() {
        let p = src.join(OsStr::from_bytes(n));
        if let Some(parent) = p.parent() {
            if fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if fs::write(&p, text(i as u64, 2000 + i * 100)).is_ok() {
            made += 1;
        }
    }
    // (a path of 900 bytes, a directory of 100 a level)
    let mut deep = src.clone();
    for k in 0..9 {
        deep.push(format!("{k}{}", "p".repeat(98)));
    }
    if fs::create_dir_all(&deep).is_ok() && fs::write(deep.join("leaf"), b"deep").is_ok() {
        made += 1;
    }
    // (times: before 1970, far on, the last nanosecond of a second)
    let at = |secs: i64, nanos: u32| match secs {
        s if s < 0 => SystemTime::UNIX_EPOCH - Duration::new(-s as u64, 0) + Duration::new(0, nanos),
        s => SystemTime::UNIX_EPOCH + Duration::new(s as u64, nanos),
    };
    let times = [(-1_000_000_000, 0), (-1, 999_999_999), (0, 1), (4_102_444_800, 999_999_999), (7_258_118_400, 5)];
    let mut want = Vec::new();
    for (k, &(s, n)) in times.iter().enumerate() {
        let p = src.join(format!("time{k}"));
        fs::write(&p, text(k as u64, 500)).unwrap();
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        if f.set_modified(at(s, n)).is_ok() {
            // (as the file system keeps it: what is compared is what it reads back)
            want.push((p.clone(), fs::metadata(&p).unwrap().modified().unwrap()));
        }
    }
    eprintln!("{made} of {} names made, {} times set", names.len() + 1, want.len());
    assert!(made > 5 && !want.is_empty(), "{made} {}", want.len());
    let before = walk(&src);
    ok(&d, &["-q", "-T", "1", "-o", "one.aqr", "src"]);
    ok(&d, &["-q", "-T", "8", "-o", "eight.aqr", "src"]);
    assert!(fs::read(d.join("one.aqr")).unwrap() == fs::read(d.join("eight.aqr")).unwrap(), "-T1 and -T8 differ");
    ok(&d, &["-q", "-d", "-o", "x", "one.aqr"]);
    assert_eq!(walk(&d.join("x")), before);
    for (p, m) in want {
        let got = fs::metadata(d.join("x").join(p.strip_prefix(&src).unwrap())).unwrap().modified().unwrap();
        assert_eq!(got, m, "{}", p.display());
    }
    ok(&d, &["-q", "-K", "one.aqr"]);
    // (appended to: nothing new, and the names are the same ones)
    let o = ok(&d, &["-a", "one.aqr", "src"]);
    assert!(stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    fs::remove_dir_all(&d).unwrap();
}

/// out/2: a write failing while extracting (a full disk) fails the run, names the file, and
/// leaves no file of part of its contents under its name; what was written whole stays.
#[cfg(unix)]
#[test]
fn extraction_write_fails() {
    let d = scratch("out2");
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a-small.txt"), text(1, 2000)).unwrap();
    fs::write(src.join("sub/big.txt"), text(2, 200_000)).unwrap();
    fs::write(src.join("z-small.txt"), text(3, 2000)).unwrap();
    let all = files(&src);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    for extra in [&[][..], &["-T", "1"], &["-f"], &["-B", "10K"]] {
        let _ = fs::remove_dir_all(d.join("x"));
        let args: Vec<&str> = ["-d", "-o", "x"].into_iter().chain(extra.iter().copied()).chain(["a.aqr"]).collect();
        let o = limited(&d, 50, &args);
        let e = stderr(&o);
        assert_eq!(o.status.code(), Some(1), "{extra:?}: {e}");
        assert!(e.contains("x/sub/big.txt") && e.contains("File too large"), "{extra:?}: {e}");
        let got = files(&d.join("x"));
        assert!(
            got.iter().all(|f| all.contains(f)),
            "{extra:?}: a file in part: {:?}",
            got.iter().map(|f| (&f.0, f.1.len())).collect::<Vec<_>>()
        );
        assert!(!got.iter().any(|f| f.0 == "sub/big.txt"), "{extra:?}");
        assert!(got.iter().all(|f| !f.0.ends_with(".part")));
    }
    // a single file's archive, written by --part
    ok(&d, &["-q", "-o", "big.aqr", "src/sub/big.txt"]);
    let o = limited(&d, 50, &["-d", "-o", "big.out", "big.aqr"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("big.out")));
    fs::remove_dir_all(&d).unwrap();
}

/// out/3: entries (directories and files) whose names mean something to a shell or another
/// system (`C:\x`, `\\server\share`, `CON`, `...`, `a..b`, `~`, `-rf`) are made inside the
/// directory extracted into under those names, whatever they hold, and nothing outside it. (Names
/// `.`, `..`, empty, with `/` or NUL are not in an archive: the encoder refuses them.)
#[cfg(unix)]
#[test]
fn hostile_names_stay_inside() {
    use aqr_archive::{Entry, EntryNode, Extra, Tree};
    use aqr_compress::{Input, Options, compress_tree};
    let names: [&[u8]; 9] =
        [b"C:\\x", b"\\\\server\\share\\y", b"CON", b"...", b"a..b", b"..a", b"~", b"-rf", b"nul.txt"];
    let d = scratch("out3");
    let e = |parent, name: &[u8], node| Entry { parent, name: name.to_vec(), node, extra: Extra::default() };
    let mut t = Tree::default();
    for n in names {
        let dir = t.entries.len();
        t.entries.push(e(None, n, EntryNode::Dir));
        t.entries.push(e(Some(dir), n, EntryNode::File(0)));
        t.entries.push(e(Some(dir), b"inner", EntryNode::Dir));
        t.entries.push(e(Some(dir + 2), b"..b", EntryNode::File(0)));
        t.entries.push(e(None, &[n, b"-file"].concat(), EntryNode::File(0)));
    }
    let contents = [b"payload".to_vec()];
    let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
    fs::write(d.join("a.aqr"), compress_tree(&t, &inputs, &Options::default()).archive).unwrap();
    // (beside it, what an escape would write over or into)
    fs::create_dir_all(d.join("w/deeper")).unwrap();
    fs::write(d.join("victim"), b"mine").unwrap();
    let o = aqr(&d.join("w/deeper"), &["-d", "-o", "../out", "../../a.aqr"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let inside = walk(&d.join("w/out"));
    assert_eq!(
        inside.len(),
        names.len() * 5,
        "{:?}",
        inside.iter().map(|f| String::from_utf8_lossy(&f.0).into_owned()).collect::<Vec<_>>()
    );
    for n in names {
        for p in [[n, b"/", n].concat(), [n, b"/inner/..b"].concat(), [n, b"-file"].concat()] {
            assert!(inside.iter().any(|f| f.0 == p && f.1 == b"payload"), "{}", String::from_utf8_lossy(&p));
        }
    }
    // (nothing made beside the directory: only what was there)
    let mut outside: Vec<_> = walk(&d).into_iter().map(|f| String::from_utf8_lossy(&f.0).into_owned()).collect();
    outside.retain(|n| !n.starts_with("w/out"));
    outside.sort();
    assert_eq!(outside, ["a.aqr", "victim", "w", "w/deeper"]);
    assert_eq!(fs::read(d.join("victim")).unwrap(), b"mine");
    fs::remove_dir_all(&d).unwrap();
}

/// The blocks of archive `a` (from its header to its footer): where each starts, its type and
/// where its data is.
fn blocks(a: &[u8]) -> Vec<(usize, u64, std::ops::Range<usize>)> {
    let leb = |at: &mut usize| {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = a[*at];
            *at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    };
    let mut out = Vec::new();
    let mut at = 6;
    loop {
        let start = at;
        let ty = leb(&mut at);
        if ty == 0x90_5bcb_957d {
            return out;
        }
        let width = 1 << (ty % 4);
        let size = u64::from_le_bytes(std::array::from_fn(|i| if i < width { a[at + i] } else { 0 }));
        at += width;
        let data = at..at + size as usize;
        at = data.end + (data.end - start) % 2;
        out.push((start, ty, data));
    }
}

/// out/4: an archive without mirror blocks damaged in one place: `-K` and `-d` name exactly the
/// files that cannot be given back (by their paths, each told however quiet, the run failing
/// with exit code 3) and give the others back; `-t` lists whatever the damage is to data, and
/// (`-t -q`, the paths alone, whatever it is) fails clearly where it is to a formula or module
/// the listing needs.
#[test]
fn damaged_files_named() {
    let d = scratch("out4");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    // (repeating beyond DEFLATE's window: recompressed, so with a module and a formula)
    fs::write(d.join("big.txt"), text(1, 80_000).repeat(8)).unwrap();
    ok(&d, &["-q", "--format", "gz", "-o", "src/big.txt.gz", "big.txt"]);
    for i in 0..5 {
        fs::write(src.join(format!("t{i}.txt")), text(10 + i, 3000 + 500 * i as usize)).unwrap();
    }
    let all = files(&src);
    ok(&d, &["-q", "-b", "2K", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    // (the blocks: two modules, formulas, big.txt.gz's data, then the data of t0 to t4)
    let types: Vec<u64> = blocks(&a).iter().map(|b| b.1).collect();
    assert_eq!(types[..3], [0x61, 0x61, 0x64], "{types:x?}");
    let bl = blocks(&a);
    let mid = |k: usize| (bl[k].2.start + bl[k].2.end) / 2;
    let data = |t: usize| bl.iter().rposition(|b| b.1 == 0x2d).unwrap() - 4 + t;
    let cases: [(&str, Vec<usize>, Vec<&str>, bool); 6] = [
        ("two files' data", vec![data(1), data(3)], vec!["t1.txt", "t3.txt"], true),
        ("a module", vec![0], vec!["big.txt.gz"], false),
        ("the other module", vec![1], vec!["big.txt.gz"], false),
        ("a formula", vec![2], vec!["big.txt.gz"], false),
        ("big.txt.gz's data", vec![6], vec!["big.txt.gz"], true),
        ("big.txt.gz's other data", vec![7], vec!["big.txt.gz"], true),
    ];
    for (what, hit, lost, listable) in cases {
        let mut b = a.clone();
        for &k in &hit {
            b[mid(k)] ^= 0x10;
        }
        fs::write(d.join("b.aqr"), &b).unwrap();
        let _ = fs::remove_dir_all(d.join("xb"));
        for args in [&["-K", "b.aqr"][..], &["-q", "-K", "b.aqr"], &["-d", "-o", "xb", "b.aqr"]] {
            let o = aqr(&d, args);
            let e = stderr(&o);
            assert_eq!(o.status.code(), Some(3), "{what} {args:?}: {e}");
            let mut told: Vec<&str> = e
                .lines()
                .filter_map(|l| l.strip_prefix("b.aqr: ")?.split(": lost").next().filter(|_| l.contains(": lost")))
                .collect();
            told.sort();
            assert_eq!(told, lost, "{what} {args:?}: {e}");
            let n = lost.len();
            assert!(e.contains(&format!("{n} file{} lost", if n == 1 { "" } else { "s" })), "{what}: {e}");
        }
        let got = files(&d.join("xb"));
        let want: Vec<_> = all.iter().filter(|f| !lost.contains(&f.0.as_str())).cloned().collect();
        assert_eq!(got, want, "{what}");
        // (the paths alone come from the metadata, which the damage is not to)
        let o = aqr(&d, &["-t", "-q", "b.aqr"]);
        assert!(o.status.success(), "{what}: {}", stderr(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 6, "{what}");
        let o = aqr(&d, &["-t", "b.aqr"]);
        if listable {
            assert!(o.status.success(), "{what}: {}", stderr(&o));
            let listing = String::from_utf8_lossy(&o.stdout).into_owned();
            assert!(all.iter().all(|f| listing.contains(&f.0)), "{what}: {listing}");
        } else {
            let e = stderr(&o);
            assert_eq!(o.status.code(), Some(3), "{what}: {e}");
            assert!(e.contains("invalid archive") && !e.contains("panicked"), "{what}: {e}");
        }
    }
    fs::remove_dir_all(&d).unwrap();
}

/// `aqr --ipc args` in `dir`, greeted: the child and the lines it tells (JSON), through a channel.
#[cfg(unix)]
fn ipc_start(
    dir: &Path,
    args: &[&str],
    answers: &[&str],
) -> (std::process::Child, std::sync::mpsc::Receiver<serde_json::Value>) {
    use std::io::{BufRead, BufReader, Write};
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(dir)
        .arg("--ipc")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    writeln!(c.stdin.as_mut().unwrap(), r#"{{"hello":1,"answers":{}}}"#, serde_json::json!(answers)).unwrap();
    let out = c.stdout.take().unwrap();
    let (to, from) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines() {
            let Ok(l) = l else { return };
            if to.send(serde_json::from_str(&l).unwrap()).is_err() {
                return;
            }
        }
    });
    (c, from)
}

#[cfg(unix)]
fn ipc_send(c: &mut std::process::Child, bytes: &[u8]) {
    use std::io::Write;
    let i = c.stdin.as_mut().unwrap();
    // (the process may be done and gone already)
    _ = i.write_all(bytes).and_then(|_| i.flush());
}

/// out/5: a frontend (`--ipc`) ending an extraction (`-d`) by `finish`, by `cancel` and by
/// closing standard input, once the first file is being written: the process ends with exit code
/// 130 and one `stopped` event (or is done, with `end`, where it was quicker), and no file
/// under its name holds part of its contents.
#[cfg(unix)]
#[test]
fn ipc_extraction_ended() {
    let d = scratch("out5");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..4 {
        fs::write(src.join(format!("big{i}.txt")), text(i, 1000).repeat(40_000)).unwrap();
    }
    let all = files(&src);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    for how in ["finish", "cancel", "eof"] {
        let _ = fs::remove_dir_all(d.join("x"));
        let (mut c, rx) = ipc_start(&d, &["-d", "-o", "x", "a.aqr"], &[]);
        assert_eq!(rx.recv().unwrap()["hello"], 1);
        assert_eq!(rx.recv().unwrap()["event"], "begin");
        let start = std::time::Instant::now();
        while walk_len(&d.join("x")) == 0 && start.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        match how {
            "finish" => ipc_send(&mut c, b"{\"control\":\"finish\"}\n"),
            "cancel" => ipc_send(&mut c, b"{\"control\":\"cancel\"}\n"),
            _ => drop(c.stdin.take()),
        }
        let rest: Vec<_> = rx.iter().filter(|v| v["event"] != "progress").collect();
        let code = c.wait().unwrap().code();
        let stopped = rest.iter().filter(|v| v["event"] == "stopped").count();
        match code {
            Some(130) => assert_eq!(stopped, 1, "{how}: {rest:?}"),
            Some(0) => assert!(stopped == 0 && rest.iter().any(|v| v["event"] == "end"), "{how}: {rest:?}"),
            c => panic!("{how}: {c:?} {rest:?}"),
        }
        let got = if d.join("x").exists() { files(&d.join("x")) } else { Vec::new() };
        assert!(
            got.iter().all(|f| all.contains(f)),
            "{how}: a file in part: {:?}",
            got.iter().map(|f| (&f.0, f.1.len())).collect::<Vec<_>>()
        );
        if code == Some(0) {
            assert_eq!(got, all);
        }
    }
    fs::remove_dir_all(&d).unwrap();
}

#[cfg(unix)]
fn walk_len(d: &Path) -> usize {
    fs::read_dir(d).map(|r| r.count()).unwrap_or(0)
}

/// The next event of `rx` that is not `progress`, within ten seconds.
#[cfg(unix)]
fn next_event(rx: &std::sync::mpsc::Receiver<serde_json::Value>) -> serde_json::Value {
    loop {
        let v = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("an event");
        if v["event"] != "progress" {
            return v;
        }
    }
}

/// An archive `a.aqr` of two files, and a directory `x` there already holding `old`.
#[cfg(unix)]
fn archive_and_directory(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), text(1, 3000)).unwrap();
    fs::write(src.join("b.txt"), text(2, 3000)).unwrap();
    ok(d, &["-q", "-o", "a.aqr", "src"]);
    fs::create_dir_all(d.join("x")).unwrap();
    fs::write(d.join("x/old"), b"old").unwrap();
    files(&src)
}

/// out/5: lines the frontend sends that are not text (nor JSON, nor known) are ignored, the
/// reader going on (a question open is answered after them); the question answered `true` is
/// written over.
#[cfg(unix)]
#[test]
fn ipc_ignores_unreadable_lines() {
    let d = scratch("out5-lines");
    let want = archive_and_directory(&d);
    let (mut c, rx) = ipc_start(&d, &["-d", "-f", "-o", "x", "a.aqr"], &["overwrite"]);
    assert_eq!(rx.recv().unwrap()["hello"], 1);
    assert_eq!(next_event(&rx)["event"], "begin");
    for line in [
        &b"\xff\xfe\xfd\n"[..],
        b"{{{\n",
        b"\n",
        b"{\"control\":\"dance\"}\n",
        b"{\"answer\":\"x\"}\n",
        b"[1,2]\n",
        b"\xc3\x28\n",
    ] {
        ipc_send(&mut c, line);
    }
    let end = next_event(&rx);
    assert_eq!(end["event"], "end", "{end}");
    assert!(c.wait().unwrap().success());
    assert_eq!(files(&d.join("x")).iter().filter(|f| want.iter().any(|w| w.0 == f.0 && w.1 == f.1)).count(), 2);
    fs::remove_dir_all(&d).unwrap();
}

/// out/5: the frontend gone (standard input closed), or cancelling, with the overwrite question
/// open: the process ends with exit code 130 and `stopped`, and what is there is left as it was.
#[cfg(unix)]
#[test]
fn ipc_gone_with_a_question_open() {
    let d = scratch("out5-ask");
    archive_and_directory(&d);
    for how in ["eof", "cancel", "finish"] {
        let (mut c, rx) = ipc_start(&d, &["-d", "-o", "x", "a.aqr"], &["overwrite"]);
        assert_eq!(rx.recv().unwrap()["hello"], 1);
        // (the question comes before the work begins, or after: either way, open)
        let mut q = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        if q["event"] == "begin" {
            q = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        }
        assert_eq!((q["kind"].as_str(), q["path"].as_str()), (Some("overwrite"), Some("x")), "{q}");
        match how {
            "eof" => drop(c.stdin.take()),
            "cancel" => ipc_send(&mut c, b"{\"control\":\"cancel\"}\n"),
            _ => ipc_send(&mut c, b"{\"control\":\"finish\"}\n"),
        }
        let rest: Vec<_> = rx.iter().filter(|v| v["event"] != "progress").collect();
        assert_eq!(c.wait().unwrap().code(), Some(130), "{how}: {rest:?}");
        assert_eq!(rest.iter().filter(|v| v["event"] == "stopped").count(), 1, "{how}: {rest:?}");
        assert_eq!(files(&d.join("x")), [("old".to_string(), b"old".to_vec())], "{how}");
    }
    fs::remove_dir_all(&d).unwrap();
}

/// out/5: paths of non-ASCII characters and spaces told as they are (in JSON) and written.
#[cfg(unix)]
#[test]
fn ipc_non_ascii_paths() {
    let d = scratch("out5-names");
    let src = d.join("원본 폴더");
    fs::create_dir_all(src.join("하위")).unwrap();
    fs::write(src.join("하위/한글 \u{1F600}.txt"), text(1, 2000)).unwrap();
    fs::write(src.join("é.txt"), text(2, 2000)).unwrap();
    let want = files(&src);
    let (mut c, rx) = ipc_start(&d, &["-c", "-o", "압축 파일.aqr", "원본 폴더"], &[]);
    assert_eq!(rx.recv().unwrap()["hello"], 1);
    let (begin, end) = (next_event(&rx), next_event(&rx));
    assert_eq!(begin["output"], "압축 파일.aqr");
    assert_eq!((end["event"].as_str(), end["files"].as_u64()), (Some("end"), Some(2)), "{end}");
    assert!(c.wait().unwrap().success());
    let (mut c, rx) = ipc_start(&d, &["-d", "-o", "출력 폴더", "압축 파일.aqr"], &[]);
    assert_eq!(rx.recv().unwrap()["hello"], 1);
    let (begin, end) = (next_event(&rx), next_event(&rx));
    assert_eq!(begin["output"], "출력 폴더");
    assert_eq!(end["event"], "end", "{end}");
    assert!(c.wait().unwrap().success());
    assert_eq!(files(&d.join("출력 폴더")), want);
    fs::remove_dir_all(&d).unwrap();
}

/// out/5: a session's requests that are wrong are answered by an error (or ignored where they
/// are not requests), the session going on to answer a right one; a `begin` without arguments
/// ends it with an error, and the frontend gone before `begin` ends it as stopped.
#[cfg(unix)]
#[test]
fn session_wrong_requests() {
    let d = scratch("out5-session");
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("src/a.txt"), b"aaaa").unwrap();
    let start = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
            .arg("--session")
            .current_dir(&d)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        ipc_send(&mut c, b"{\"hello\":1,\"answers\":[]}\n");
        c
    };
    let events = |c: &mut std::process::Child| {
        use std::io::{BufRead, BufReader};
        let out = c.stdout.take().unwrap();
        let (to, from) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if to.send(serde_json::from_str::<serde_json::Value>(&l).unwrap()).is_err() {
                    return;
                }
            }
        });
        from
    };
    let mut c = start();
    let rx = events(&mut c);
    assert_eq!(rx.recv().unwrap()["hello"], 1);
    for (req, error) in [
        (&br#"{"control":"inventory","id":1}"#[..], "without args"),
        (br#"{"control":"inventory","id":2,"args":[1,{"a":2}]}"#, "strings"),
        (br#"{"control":"inventory","id":3,"args":["--nope"]}"#, "unknown option"),
        (br#"{"control":"inventory","id":4,"args":["-c"]}"#, "no input"),
        (b"{\"control\":\"inventory\",\"id\":5,\"args\":[\"-c\",\"--\",\"a\\u0000b\"]}", "NUL"),
    ] {
        ipc_send(&mut c, req);
        ipc_send(&mut c, b"\n");
        let v = next_event(&rx);
        assert!(v["event"] == "inventory" && v["done"] == true, "{v}");
        assert!(v["error"].as_str().is_some_and(|e| e.contains(error)), "{error}: {v}");
        assert_eq!(v["id"], req_id(req), "{v}");
    }
    // (not requests: ignored)
    for junk in [&b"{\"control\":\"dance\",\"id\":6}\n"[..], b"{{{\n", b"\xff\xfe\n", b"{\"control\":7}\n"] {
        ipc_send(&mut c, junk);
    }
    ipc_send(&mut c, br#"{"control":"inventory","id":9,"args":["-c","--","src"]}"#);
    ipc_send(&mut c, b"\n");
    let v = next_event(&rx);
    assert_eq!(
        (v["id"].as_u64(), v["files"].as_u64(), &v["done"]),
        (Some(9), Some(1), &serde_json::json!(true)),
        "{v}"
    );
    // a begin of no args ends the session
    ipc_send(&mut c, b"{\"control\":\"begin\"}\n");
    // (standard input kept open: `wait` would close it, the frontend gone)
    let _stdin = c.stdin.take();
    let status = c.wait().unwrap();
    let mut err = String::new();
    std::io::Read::read_to_string(&mut c.stderr.take().unwrap(), &mut err).unwrap();
    assert_eq!(status.code(), Some(1), "{err}");
    assert!(err.contains("a request without args"), "{err}");
    // the frontend gone before it begins
    let mut c = start();
    let rx = events(&mut c);
    assert_eq!(rx.recv().unwrap()["hello"], 1);
    drop(c.stdin.take());
    let rest: Vec<_> = rx.iter().collect();
    assert_eq!(c.wait().unwrap().code(), Some(130));
    assert!(rest.iter().any(|v| v["event"] == "stopped"), "{rest:?}");
    fs::remove_dir_all(&d).unwrap();
}

#[cfg(unix)]
fn req_id(req: &[u8]) -> u64 {
    let s = String::from_utf8_lossy(req);
    let at = s.find("\"id\":").unwrap() + 5;
    s[at..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

/// out/6: an archive of no files is extracted (an empty directory made, by `-o`, by its name,
/// and told where there is one already), tested and listed.
#[test]
fn extracting_an_archive_of_no_files() {
    let d = scratch("out6-zero");
    fs::create_dir_all(d.join("empty/sub")).unwrap();
    ok(&d, &["-q", "-o", "z.aqr", "empty"]);
    let o = ok(&d, &["-d", "-o", "zo", "z.aqr"]);
    assert!(stderr(&o).contains("0 files"), "{}", stderr(&o));
    assert!(d.join("zo").is_dir() && fs::read_dir(d.join("zo")).unwrap().count() == 1);
    let o = aqr(&d, &["-d", "-o", "zo", "z.aqr"]);
    assert!(!o.status.success() && stderr(&o).contains("zo exists (-f to overwrite)"), "{}", stderr(&o));
    ok(&d, &["-d", "-f", "-o", "zo", "z.aqr"]);
    ok(&d, &["-d", "z.aqr"]);
    assert!(d.join("z").is_dir());
    assert!(stderr(&ok(&d, &["-K", "z.aqr"])).contains("0 files"));
    assert!(String::from_utf8_lossy(&ok(&d, &["-t", "-q", "z.aqr"]).stdout).contains("sub"));
    fs::remove_dir_all(&d).unwrap();
}

/// out/6: modification times at the extremes of what an archive can hold (the least and greatest
/// seconds, a nanosecond short of the next second, millennia off) are applied as far as the file
/// system takes them, on the right side of 1970 and not wrapped round; nothing fails or panics.
#[cfg(unix)]
#[test]
fn extracting_extreme_times() {
    use aqr_archive::{Entry, EntryNode, Extra, Time, Tree};
    use aqr_compress::{Input, Options, compress_tree};
    let d = scratch("out6-times");
    for (secs, nanos) in
        [(i64::MIN, 0), (i64::MAX, 999_999_999), (-1, 999_999_999), (1 << 40, 5), (-(1 << 40), 5), (i64::MIN / 2, 1)]
    {
        let extra = Extra { mtime: Some(Time::new(secs, nanos)), ..Extra::default() };
        let entry = Entry { parent: None, name: b"f".to_vec(), node: EntryNode::File(0), extra };
        let t = Tree { root: Extra::default(), entries: vec![entry] };
        let contents = [b"x".to_vec()];
        let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
        fs::write(d.join("t.aqr"), compress_tree(&t, &inputs, &Options::default()).archive).unwrap();
        let _ = fs::remove_dir_all(d.join("tx"));
        let o = aqr(&d, &["-d", "-o", "tx", "t.aqr"]);
        assert!(o.status.success() && !stderr(&o).contains("panicked"), "{secs} {nanos}: {}", stderr(&o));
        assert_eq!(fs::read(d.join("tx/f")).unwrap(), b"x");
        let m = fs::metadata(d.join("tx/f")).unwrap().modified().unwrap();
        let after_1970 = m >= std::time::UNIX_EPOCH;
        assert_eq!(after_1970, secs >= 0, "{secs} {nanos}: {m:?}");
        if secs == -1 {
            assert_eq!(m, std::time::UNIX_EPOCH - std::time::Duration::new(0, 1));
        }
    }
    fs::remove_dir_all(&d).unwrap();
}

/// out/6: a destination that cannot be written (a read-only directory, a parent that is a file,
/// the output a file there already) fails with the path and the reason, nothing made.
#[cfg(unix)]
#[test]
fn unwritable_destinations() {
    use std::os::unix::fs::PermissionsExt;
    let d = scratch("out6-dest");
    fs::write(d.join("f.txt"), b"x").unwrap();
    ok(&d, &["-q", "-o", "a.aqr", "f.txt"]);
    fs::create_dir_all(d.join("ro")).unwrap();
    fs::set_permissions(d.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    // (root writes anywhere)
    let root = fs::write(d.join("ro/probe"), b"").is_ok();
    for (args, why) in [
        (&["-d", "-o", "ro/out", "a.aqr"][..], "Permission denied"),
        (&["-d", "-f", "-o", "ro", "a.aqr"], ""),
        (&["-d", "-o", "f.txt/sub", "a.aqr"], "Not a directory"),
        (&["-d", "-o", "f.txt", "a.aqr"], "f.txt exists (-f to overwrite)"),
    ] {
        let o = aqr(&d, args);
        let e = stderr(&o);
        if args[1] == "-o" && args[2] == "ro/out" && root {
            continue;
        }
        assert_eq!(o.status.code(), Some(1), "{args:?}: {e}");
        assert!(e.starts_with("aqr: ") && e.contains(why) && !e.contains("panicked"), "{args:?}: {e}");
    }
    assert!(!d.join("ro/out").exists() && !d.join("f.txt/sub").exists());
    assert_eq!(fs::read(d.join("f.txt")).unwrap(), b"x");
    assert_eq!(fs::read_dir(d.join("ro")).unwrap().count(), root as usize);
    fs::set_permissions(d.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(&d).unwrap();
}

/// out/6: `--stats` on archives damaged (a byte flipped, cut short, not an archive): the run ends
/// as it does without it (exit code 3, the files lost named), and the stats files are written, of
/// the run finished, valid.
#[test]
fn stats_on_damaged_archives() {
    let d = scratch("out6-stats");
    source(&d, 4);
    ok(&d, &["-q", "-o", "s.aqr", "src"]);
    let a = fs::read(d.join("s.aqr")).unwrap();
    let flipped = {
        let mut b = a.clone();
        b[a.len() / 3] ^= 1;
        b
    };
    for (what, bytes) in [("flipped", flipped), ("cut", a[..a.len() / 2].to_vec()), ("junk", vec![7; 100])] {
        fs::write(d.join("sd.aqr"), &bytes).unwrap();
        for args in [&["-K"][..], &["-d", "-f", "-o", "sdx"]] {
            for stats in ["--stats=st.json", "--stats=st.html"] {
                let _ = fs::remove_file(d.join("st.json"));
                let _ = fs::remove_file(d.join("st.html"));
                let mut a: Vec<&str> = args.to_vec();
                a.extend([stats, "sd.aqr"]);
                let o = aqr(&d, &a);
                let e = stderr(&o);
                assert_eq!(o.status.code(), Some(3), "{what} {a:?}: {e}");
                assert!(e.contains("stats written to") && !e.contains("panicked"), "{what} {a:?}: {e}");
                let name = &stats[8..];
                let written = fs::read_to_string(d.join(name)).unwrap();
                if name.ends_with("json") {
                    let v: serde_json::Value = serde_json::from_str(&written).unwrap();
                    assert_eq!(v["state"], "finished", "{what} {a:?}");
                } else {
                    assert!(written.contains("</html>"), "{what} {a:?}");
                }
            }
        }
    }
    fs::remove_dir_all(&d).unwrap();
}
