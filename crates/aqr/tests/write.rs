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
/// replaced (`-f`) or appended to, it is an input like any file, read as it was.
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
    ok(&d, &["-q", "-a", "src/a.aqr", "src"]);
    let got = extracted(&d, "src/a.aqr");
    assert_eq!(got.iter().find(|f| f.0 == "a.aqr").unwrap().1, second);
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
