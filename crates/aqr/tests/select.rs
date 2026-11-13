//! Some paths of an archive extracted alone (`aqr -d ARCHIVE PATH [--rename TO]...`): in place,
//! with the directories on the way, or renamed; a directory with what is under it; only what
//! they need decoded; and the errors of paths given wrong. Of Aqueous archives and of others.
//! Some paths of an Aqueous archive listed alone (`aqr -t ARCHIVE PATH...`) too.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    common::scratch("select", name)
}

/// `aqr` with `args`, run in directory `dir`: whether it succeeded, and its standard error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String) {
    let o: Output = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// The paths under `d` (but `but`), sorted, as `/`-separated relative paths.
fn listing(d: &Path, but: &[&str]) -> Vec<String> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            if fs::symlink_metadata(&p).unwrap().is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(d, d, &mut out);
    out.retain(|p| !but.iter().any(|b| p == b || p.starts_with(&format!("{b}/"))));
    out.sort();
    out
}

fn read(p: impl AsRef<Path>) -> Vec<u8> {
    fs::read(p.as_ref()).unwrap_or_else(|e| panic!("{}: {e}", p.as_ref().display()))
}

/// A directory archived (each file a block of its own, stored as it is), paths of it
/// extracted alone.
#[test]
fn aqueous() {
    let d = scratch("aqr");
    let src = d.join("src");
    fs::create_dir_all(src.join("d/e")).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::write(src.join("d/b.txt"), b"b").unwrap();
    fs::write(src.join("d/e/c.txt"), b"c").unwrap();
    let big: Vec<u8> = [&b"MARKER"[..], &(0..=255).collect::<Vec<u8>>().repeat(400)].concat();
    fs::write(src.join("big.bin"), &big).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("b.txt", src.join("d/m")).unwrap();
        std::os::unix::fs::symlink("../a.txt", src.join("d/l")).unwrap();
        std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
    }
    let (ok, err) = aqr_in(&d, &["-q", "-l", "0", "-b", "1", "-o", "arch.aqr", "src"]);
    assert!(ok, "{err}");
    let run = |args: &[&str]| {
        let w = d.join("w");
        if w.exists() {
            fs::remove_dir_all(&w).unwrap();
        }
        fs::create_dir(&w).unwrap();
        let (ok, err) = aqr_in(&w, args);
        (ok, err, w)
    };

    // in place: where the whole archive would have it (into its name), the directories on the
    // way made; or in -o
    let (ok, err, w) = run(&["-d", "../arch.aqr", "d/b.txt"]);
    assert!(ok, "{err}");
    assert!(!w.join("arch").exists(), "beside the archive, not here");
    assert_eq!(listing(&d.join("arch"), &[]), ["d", "d/b.txt"]);
    assert_eq!(read(d.join("arch/d/b.txt")), b"b");
    fs::remove_dir_all(d.join("arch")).unwrap();
    let (ok, err, w) = run(&["-d", "../arch.aqr", "d/e", "a.txt", "-o", "out"]);
    assert!(ok, "{err}");
    assert_eq!(listing(&w, &[]), ["out", "out/a.txt", "out/d", "out/d/e", "out/d/e/c.txt"]);

    // renamed: a file (over one there only with -f), a directory with what is under it (its
    // links leading out of it not made), the directories before it made; several at once
    let (ok, err, w) = run(&["-d", "../arch.aqr", "a.txt", "--rename", "x.txt"]);
    assert!(ok, "{err}");
    assert_eq!(listing(&w, &[]), ["x.txt"]);
    assert_eq!(read(w.join("x.txt")), b"a");
    let (ok, err) = aqr_in(&w, &["-d", "../arch.aqr", "d/b.txt", "--rename", "x.txt"]);
    assert!(!ok && err.contains("exists (-f to overwrite)"), "{err}");
    let (ok, err) = aqr_in(&w, &["-d", "-f", "../arch.aqr", "d/b.txt", "--rename", "x.txt"]);
    assert!(ok, "{err}");
    assert_eq!(read(w.join("x.txt")), b"b");
    let (ok, err, w) = run(&["-d", "../arch.aqr", "d", "--rename", "p/q/dd", "big.bin", "--rename", "big"]);
    assert!(ok, "{err}");
    assert_eq!(read(w.join("p/q/dd/b.txt")), b"b");
    assert_eq!(read(w.join("p/q/dd/e/c.txt")), b"c");
    assert_eq!(read(w.join("big")), big);
    #[cfg(unix)]
    {
        assert_eq!(fs::read_link(w.join("p/q/dd/m")).unwrap(), Path::new("b.txt"));
        assert!(err.contains("a link reaching out of p/q/dd") && fs::symlink_metadata(w.join("p/q/dd/l")).is_err());
        assert_eq!(
            listing(&w, &[]),
            ["big", "p", "p/q", "p/q/dd", "p/q/dd/b.txt", "p/q/dd/e", "p/q/dd/e/c.txt", "p/q/dd/m"]
        );
        // (with --links all, made as it is too)
        let (ok, err, w) = run(&["-d", "../arch.aqr", "d", "--rename", "p/q/dd", "--links", "all"]);
        assert!(ok && !err.contains("a link"), "{err}");
        assert_eq!(fs::read_link(w.join("p/q/dd/l")).unwrap(), Path::new("../a.txt"));
        let (ok, err) = aqr_in(&w, &["-d", "../arch.aqr", "--links", "some"]);
        assert!(!ok && err.contains("--links takes inside or all: some"), "{err}");
        // (a link renamed itself: made as it is)
        let (ok, err, w) = run(&["-d", "../arch.aqr", "link", "--rename", "lk"]);
        assert!(ok, "{err}");
        assert_eq!(fs::read_link(w.join("lk")).unwrap(), Path::new("a.txt"));
    }

    // tested: the files of the paths alone
    let (ok, err, _) = run(&["-K", "../arch.aqr", "d"]);
    assert!(ok && err.contains("(tested): 2 files"), "{err}");

    // only what they need decoded: a damaged block of another file fails the archive whole,
    // not them
    let mut bad = read(d.join("arch.aqr"));
    let at = bad.windows(6).position(|w| w == b"MARKER").unwrap();
    bad[at + 1000] ^= 0x55;
    fs::write(d.join("bad.aqr"), &bad).unwrap();
    let (ok, err, _) = run(&["-K", "../bad.aqr"]);
    assert!(!ok && err.contains("checksum mismatch"), "{err}");
    let (ok, err, w) = run(&["-d", "../bad.aqr", "d", "--rename", "dd", "a.txt", "--rename", "a"]);
    assert!(ok, "{err}");
    assert_eq!((read(w.join("dd/e/c.txt")), read(w.join("a"))), (b"c".to_vec(), b"a".to_vec()));

    // paths given wrong: nothing made
    for (args, why) in [
        (&["-d", "--rename", "z", "../arch.aqr"][..], "--rename z not after a path of an archive"),
        (&["-d", "../arch.aqr", "--rename", "z"], "--rename z not after a path of an archive"),
        (&["-d", "../arch.aqr", "a.txt", "--rename", "y", "--rename", "z"], "a.txt: --rename given twice"),
        (&["-d", "../arch.aqr", "nope", "a.txt"], "nope: not in the archive"),
        (&["-d", "../arch.aqr", "d/e/x"], "d/e/x: not in the archive"),
        (&["-d", "../arch.aqr", "d", "d/b.txt"], "d/b.txt: under d, given too"),
        (&["-d", "../arch.aqr", "a.txt", "./a.txt"], "a.txt: given twice"),
        (&["-d", "../arch.aqr", "d/../a.txt"], "d/../a.txt: not a path in an archive"),
        (&["-d", "--auto-dir", "../arch.aqr", "a.txt"], "--auto-dir with paths of an archive"),
        (&["-c", "--rename", "q", "../src"], "--rename q not after an input"),
    ] {
        let (ok, err, w) = run(args);
        assert!(!ok && err.contains(why), "{args:?}: {err}");
        assert!(listing(&w, &[]).is_empty() && !d.join("arch").exists(), "{args:?}");
    }
    // an archive of a single file has no paths
    let (ok, err) = aqr_in(&d, &["-q", "src/a.txt", "-o", "one.aqr"]);
    assert!(ok, "{err}");
    let (ok, err, _) = run(&["-d", "../one.aqr", "a.txt"]);
    assert!(!ok && err.contains("an archive of a single file"), "{err}");

    // listed: the entries at the paths and under them alone; several archives with --each
    let list = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(&d).args(args).output().unwrap();
        let (out, err) = (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr));
        assert!(o.status.success(), "{args:?}: {err}");
        out
    };
    #[cfg(unix)]
    let (all, d_all) =
        ("a.txt\nbig.bin\nd/\nd/b.txt\nd/e/\nd/e/c.txt\nd/l\nd/m\nlink\n", "d/\nd/b.txt\nd/e/\nd/e/c.txt\nd/l\nd/m\n");
    #[cfg(not(unix))]
    let (all, d_all) = ("a.txt\nbig.bin\nd/\nd/b.txt\nd/e/\nd/e/c.txt\n", "d/\nd/b.txt\nd/e/\nd/e/c.txt\n");
    assert_eq!(list(&["-t", "-q", "arch.aqr"]), all);
    assert_eq!(list(&["-t", "-q", "arch.aqr", "d"]), d_all);
    assert_eq!(list(&["-t", "-q", "arch.aqr", "d/e", "./a.txt"]), "a.txt\nd/e/\nd/e/c.txt\n");
    let t = list(&["-t", "arch.aqr", "d/e"]);
    assert!(t.contains("d/e/c.txt") && !t.contains("b.txt") && t.contains("1 file, 1 directory"), "{t}");
    // (each row as the whole archive's listing has it: its size, time, flags and instructions)
    let words = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let whole: Vec<String> = list(&["-t", "arch.aqr"]).lines().map(words).collect();
    for l in t.lines().filter(|l| l.contains("d/e")) {
        assert!(whole.contains(&words(l)), "{l}\n{whole:#?}");
    }
    let t = list(&["-t", "-v", "arch.aqr", "d/b.txt"]);
    assert!(t.contains("d/b.txt") && t.lines().last().unwrap().contains("1 file"), "{t}");
    assert_eq!(list(&["-t", "-q", "--each", "arch.aqr", "arch.aqr"]), format!("{all}{all}"));
    for (args, why) in [
        (&["-t", "arch.aqr", "nope"][..], "nope: not in the archive"),
        (&["-t", "arch.aqr", "d", "d/b.txt"], "d/b.txt: under d, given too"),
        (&["-t", "arch.aqr", "a.txt", "--rename", "z"], "-t takes an archive and its paths alone"),
        (&["-t", "one.aqr", "a.txt"], "an archive of a single file"),
    ] {
        let (ok, err) = aqr_in(&d, args);
        assert!(!ok && err.contains(why), "{args:?}: {err}");
    }

    // --each: every input an archive, extracted whole
    fs::copy(d.join("arch.aqr"), d.join("w2.aqr")).unwrap();
    let (ok, err, _) = run(&["-q", "-d", "--each", "../arch.aqr", "../w2.aqr"]);
    assert!(ok, "{err}");
    assert_eq!(read(d.join("arch/d/e/c.txt")), b"c");
    assert_eq!(read(d.join("w2/big.bin")), big);
    fs::remove_dir_all(&d).unwrap();
}

/// A fixture of aqr-unpack's (or a codec crate's) copied into `d`.
fn fixture(d: &Path, from: &str) -> String {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(from);
    let name = from.file_name().unwrap().to_string_lossy().into_owned();
    fs::copy(&from, d.join(&name)).unwrap();
    name
}

/// Archives of other formats: paths extracted alone, what is left out of the rest not told.
#[test]
fn others() {
    let d = scratch("others");
    let zip = fixture(&d, "aqr-unpack/tests/data/tree.zip");
    let tgz = fixture(&d, "aqr-unpack/tests/data/tree.tar.gz");
    let rar = fixture(&d, "aqr-unpack/tests/data/tree-rar5.rar");
    let sz = fixture(&d, "../codecs/aqr-7z/tests/data/blocks.7z");
    let fresh = |name: &str| {
        let w = d.join(name);
        fs::create_dir(&w).unwrap();
        w
    };
    // (ZIP's `../evil.txt` left out, and told where all is extracted: not here)
    let w = fresh("zip");
    let (ok, err) = aqr_in(&w, &["-d", &format!("../{zip}"), "dir", "--rename", "got", "stored.txt"]);
    assert!(ok, "{err}");
    assert!(!err.contains("evil") && err.contains("-> .:"), "{err}");
    assert_eq!(listing(&w, &[]), ["got", "got/in.txt", "stored.txt"]);
    assert_eq!(read(w.join("got/in.txt")), b"in dir");
    // tar: read through, the paths alone made; one not there an error once it is read
    let w = fresh("tgz");
    let (ok, err) = aqr_in(&w, &["-d", &format!("../{tgz}"), "dir/file.txt", "한글", "-o", "t"]);
    assert!(ok, "{err}");
    assert_eq!(listing(&w, &[]), ["t", "t/dir", "t/dir/file.txt", "t/한글", "t/한글/파일.txt"]);
    assert_eq!(read(w.join("t/한글/파일.txt")), "유니코드".as_bytes());
    let (ok, err) = aqr_in(&w, &["-d", &format!("../{tgz}"), "nope"]);
    assert!(!ok && err.contains("nope: not in the archive"), "{err}");
    // RAR5's hard link, left out: told so, not "not in the archive"
    let (ok, err) = aqr_in(&w, &["-d", &format!("../{rar}"), "dir/hard"]);
    assert!(!ok && err.contains("dir/hard:") && !err.contains("not in the archive"), "{err}");
    // 7z (solid): a file renamed, as it is extracted whole
    let w = fresh("7z");
    let (ok, err) = aqr_in(&w, &["-q", "-d", &format!("../{sz}"), "-o", "all"]);
    assert!(ok, "{err}");
    let (ok, err) = aqr_in(&w, &["-d", &format!("../{sz}"), "bin/noise.dat", "--rename", "n"]);
    assert!(ok, "{err}");
    assert_eq!(read(w.join("n")), read(w.join("all/bin/noise.dat")));
    fs::remove_dir_all(&d).unwrap();
}
