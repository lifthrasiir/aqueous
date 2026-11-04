//! `--rm`: the inputs removed once their archive is made, an archive once it is extracted whole;
//! what was not archived, or changed since it was read, kept and told, the run failing.

use std::fs;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-rm-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// `aqr -q` with `args`, run in directory `dir`: whether it succeeded, and its standard error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).arg("-q").args(args).output();
    let o = o.unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// The paths under `d`, sorted, as `/`-separated relative paths, files with their contents.
fn listing(d: &Path) -> Vec<String> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let name = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                out.push(format!("{name}/"));
                walk(root, &p, out);
            // (the archives at the top by their names alone)
            } else if m.is_file() && (name.contains('/') || !name.ends_with(".aqr") && !name.ends_with(".zip")) {
                out.push(format!("{name}={}", String::from_utf8_lossy(&fs::read(&p).unwrap())));
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(d, d, &mut out);
    out.sort();
    out
}

/// A file and a directory, each compressed and removed, then extracted and the archive removed,
/// as Aqueous archives and as ZIP; several inputs into one archive (`-o`), each removed.
#[test]
fn removed_and_back() {
    let d = scratch("back");
    fs::write(d.join("f"), b"one").unwrap();
    fs::create_dir_all(d.join("t/s")).unwrap();
    fs::write(d.join("t/a"), b"two").unwrap();
    fs::write(d.join("t/s/b"), b"three").unwrap();
    let before = listing(&d);

    assert_eq!(aqr_in(&d, &["--rm", "f", "t"]), (true, String::new()));
    assert_eq!(listing(&d), ["f.aqr", "t.aqr"]);
    assert_eq!(aqr_in(&d, &["-d", "--rm", "f.aqr", "--each", "t.aqr"]), (true, String::new()));
    assert_eq!(listing(&d), before);

    assert_eq!(aqr_in(&d, &["--rm", "--format", "zip", "t"]), (true, String::new()));
    assert_eq!(listing(&d), ["f=one", "t.zip"]);
    assert_eq!(aqr_in(&d, &["-d", "--rm", "t.zip"]), (true, String::new()));
    assert_eq!(listing(&d), before);

    assert_eq!(aqr_in(&d, &["--rm", "-o", "x.aqr", "f", "t/s"]), (true, String::new()));
    assert_eq!(listing(&d), ["t/", "t/a=two", "x.aqr"]);
    assert_eq!(aqr_in(&d, &["-d", "--rm", "-o", "x", "x.aqr"]), (true, String::new()));
    assert_eq!(listing(&d), ["t/", "t/a=two", "x/", "x/f=one", "x/t/", "x/t/s/", "x/t/s/b=three"]);
    fs::remove_dir_all(&d).unwrap();
}

/// The archive written over an input of its own (`-f -o d/x.aqr d`) is not removed with it;
/// nor is an archive extracted over itself.
#[test]
fn output_in_place_kept() {
    let d = scratch("place");
    fs::create_dir_all(d.join("d")).unwrap();
    fs::write(d.join("d/a"), b"a").unwrap();
    fs::write(d.join("d/x.aqr"), b"old").unwrap();
    let (ok, err) = aqr_in(&d, &["-f", "--rm", "-o", "d/x.aqr", "d"]);
    // (the path as found under `d`, of the system's separator)
    let kept = format!("kept {} (changed since it was read)", Path::new("d").join("x.aqr").display());
    assert!(!ok && err.contains(&kept), "{err}");
    assert_eq!(listing(&d).len(), 2);
    assert!(listing(&d)[1].starts_with("d/x.aqr=") && fs::read(d.join("d/x.aqr")).unwrap() != b"old");
    fs::rename(d.join("d/x.aqr"), d.join("x.aqr")).unwrap();
    fs::remove_dir(d.join("d")).unwrap();
    assert_eq!(aqr_in(&d, &["-d", "x.aqr"]), (true, String::new()));
    assert_eq!(listing(&d), ["x.aqr", "x/", "x/a=a", "x/x.aqr=old"]);
    fs::remove_dir_all(d.join("x")).unwrap();
    fs::remove_file(d.join("x.aqr")).unwrap();

    // a single file decompressed over its archive
    fs::write(d.join("f"), b"f").unwrap();
    assert_eq!(aqr_in(&d, &["f"]), (true, String::new()));
    let (ok, err) = aqr_in(&d, &["-d", "-f", "--rm", "-o", "f.aqr", "f.aqr"]);
    assert!(!ok && err.contains("f.aqr not removed"), "{err}");
    assert_eq!(fs::read(d.join("f.aqr")).unwrap(), b"f");
    fs::remove_dir_all(&d).unwrap();
}

/// What was not archived (a socket) is kept, the directories holding it too, told once; the
/// rest is removed.
#[cfg(unix)]
#[test]
fn not_archived_kept() {
    let d = scratch("socket");
    fs::create_dir_all(d.join("t/s/u")).unwrap();
    fs::write(d.join("t/a"), b"a").unwrap();
    fs::write(d.join("t/s/u/b"), b"b").unwrap();
    let _l = std::os::unix::net::UnixListener::bind(d.join("t/s/sock")).unwrap();
    let (ok, err) = aqr_in(&d, &["--rm", "t"]);
    assert!(!ok, "{err}");
    assert_eq!(
        err.lines().filter(|l| l.starts_with("kept ")).collect::<Vec<_>>(),
        ["kept t/s (not empty: something else is in it)"]
    );
    assert_eq!(listing(&d), ["t.aqr", "t/", "t/s/", "t/s/sock"]);
    fs::remove_dir_all(&d).unwrap();
}

/// `--rm` where nothing would be removed whole: testing, listing, some paths of an archive.
#[test]
fn refused() {
    let d = scratch("refused");
    fs::write(d.join("f"), b"f").unwrap();
    assert_eq!(aqr_in(&d, &["-o", "x.aqr", "f"]), (true, String::new()));
    for args in [&["-K", "--rm", "x.aqr"][..], &["-t", "--rm", "x.aqr"], &["-d", "--rm", "x.aqr", "f"]] {
        let (ok, err) = aqr_in(&d, args);
        assert!(!ok && err.contains("--rm"), "{args:?}: {err}");
    }
    assert_eq!(listing(&d), ["f=f", "x.aqr"]);
    fs::remove_dir_all(&d).unwrap();
}
