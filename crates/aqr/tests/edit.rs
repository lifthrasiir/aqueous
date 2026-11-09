//! Archives edited in place: paths taken out (`--delete`) and moved (`--move`), the archive
//! written again of what it holds (`--compact`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-edit-{name}-{}", std::process::id()));
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

/// A directory `src` under `d`: text files at the top and in `sub/` and `sub/deep/`.
fn source(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(src.join("sub/deep")).unwrap();
    for i in 0..9u64 {
        let at = ["", "sub/", "sub/deep/"][i as usize % 3];
        fs::write(src.join(format!("{at}t{i}.txt")), text(10 + i, 3000 + 700 * i as usize)).unwrap();
    }
    files(&src)
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

/// `want` but for the files at or under the paths `gone`.
fn without(want: &[(String, Vec<u8>)], gone: &[&str]) -> Vec<(String, Vec<u8>)> {
    let under = |p: &str| gone.iter().any(|g| p == *g || p.starts_with(&format!("{g}/")));
    want.iter().filter(|(p, _)| !under(p)).cloned().collect()
}

/// Paths taken out of an archive, one after another: what is left extracted, the archive grown
/// by a little each time; again after a stub before it, and with appending after.
#[test]
fn deleted() {
    let d = scratch("deleted");
    let want = source(&d);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    fs::write(d.join("lead.aqr"), [b"#!stub\n".as_slice(), &a].concat()).unwrap();
    for name in ["a.aqr", "lead.aqr"] {
        let len = || fs::metadata(d.join(name)).unwrap().len();
        let before = len();
        ok(&d, &["-q", "--delete", name, "sub/deep", "t0.txt"]);
        assert!(len() > before && len() < before + 200, "{} -> {}", before, len());
        assert_eq!(extracted(&d, name), without(&want, &["sub/deep", "t0.txt"]));
        ok(&d, &["-q", "--delete", name, "sub/t1.txt"]);
        assert_eq!(extracted(&d, name), without(&want, &["sub/deep", "t0.txt", "sub/t1.txt"]));
        ok(&d, &["-K", name]);
    }
    // what was taken out appended again
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    assert_eq!(extracted(&d, "a.aqr"), want);
    fs::remove_dir_all(&d).unwrap();
}

/// A path not in the archive, or none given, is an error, the archive left as it is; so is an
/// encrypted archive.
#[test]
fn delete_refused() {
    let d = scratch("refused");
    source(&d);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    for args in [
        &["--delete", "a.aqr", "t0.txt", "nope"][..],
        &["--delete", "a.aqr"],
        &["--delete", "-o", "b", "a.aqr", "t0.txt"],
    ] {
        let o = aqr(&d, args);
        assert!(!o.status.success(), "{args:?}");
        assert_eq!(fs::read(d.join("a.aqr")).unwrap(), a, "{args:?}");
    }
    let o = aqr(&d, &["--delete", "a.aqr", "t0.txt", "nope"]);
    assert!(stderr(&o).contains("no nope in it"), "{}", stderr(&o));
    fs::write(d.join("pass"), "secret\n").unwrap();
    ok(&d, &["-q", "--passphrase-from", "pass", "-o", "e.aqr", "src"]);
    let o = aqr(&d, &["--delete", "e.aqr", "t0.txt"]);
    assert!(stderr(&o).contains("encrypted"), "{}", stderr(&o));
    fs::remove_dir_all(&d).unwrap();
}

/// An archive appended to, synchronized and taken from, written again: what it holds the same,
/// the file smaller than it was and about as one made of it anew; a stub before it kept; a link
/// to it left a link. Written again once more, the same bytes.
#[test]
fn compacted() {
    let d = scratch("compacted");
    let want = source(&d);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    // (a file changed, one gone, one taken out: three segments)
    fs::write(d.join("src/t2.txt"), text(99, 9000)).unwrap();
    fs::remove_file(d.join("src/sub/t4.txt")).unwrap();
    ok(&d, &["-q", "-s", "a.aqr", "src"]);
    ok(&d, &["-q", "--delete", "a.aqr", "sub/deep/t5.txt"]);
    let held = extracted(&d, "a.aqr");
    let mut now = files(&d.join("src"));
    now.retain(|(p, _)| p != "sub/deep/t5.txt");
    assert_eq!(held, now);
    assert_ne!(held, want);
    let a = fs::read(d.join("a.aqr")).unwrap();
    fs::write(d.join("lead.aqr"), [b"#!stub\n".as_slice(), &a].concat()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("lead.aqr", d.join("link.aqr")).unwrap();
    let len = |n: &str| fs::metadata(d.join(n)).unwrap().len();
    ok(&d, &["-q", "--compact", "a.aqr"]);
    ok(&d, &["-K", "a.aqr"]);
    assert_eq!(extracted(&d, "a.aqr"), held);
    assert!(len("a.aqr") < a.len() as u64, "{} -> {}", a.len(), len("a.aqr"));
    let compacted = fs::read(d.join("a.aqr")).unwrap();
    ok(&d, &["-q", "--compact", "a.aqr"]);
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), compacted);
    let link = if cfg!(unix) { "link.aqr" } else { "lead.aqr" };
    ok(&d, &["-q", "--compact", link]);
    assert!(fs::symlink_metadata(d.join(link)).unwrap().file_type().is_symlink() == cfg!(unix));
    assert_eq!(fs::read(d.join("lead.aqr")).unwrap(), [b"#!stub\n".as_slice(), &compacted].concat());
    // taken out and written again at once: no tombstone, nothing of it left
    ok(&d, &["-q", "--delete", "--compact", "a.aqr", "sub"]);
    let mut left = held.clone();
    left.retain(|(p, _)| !p.starts_with("sub/"));
    assert_eq!(extracted(&d, "a.aqr"), left);
    let o = ok(&d, &["-I", "a.aqr"]);
    let info = String::from_utf8_lossy(&o.stdout);
    assert!(info.contains("segments: 1") || info.contains("segments: 2"), "{info}");
    assert!(!info.contains("tombstone"), "{info}");
    assert!(d.read_dir().unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
    fs::remove_dir_all(&d).unwrap();
}

/// Written again, an archive of nothing left, an encrypted one, one of mirror blocks and one of
/// a single file are refused, each left as it is; so is --compact given paths.
#[test]
fn compact_refused() {
    let d = scratch("compact-refused");
    source(&d);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    fs::write(d.join("pass"), "secret\n").unwrap();
    ok(&d, &["-q", "--passphrase-from", "pass", "-o", "e.aqr", "src"]);
    ok(&d, &["-q", "-A", "-o", "m.aqr", "src"]);
    ok(&d, &["-q", "-o", "one.aqr", "src/t0.txt"]);
    for (args, told) in [
        (&["--delete", "--compact", "a.aqr", "t0.txt", "t3.txt", "t6.txt", "sub"][..], "nothing left"),
        (&["--compact", "e.aqr"], "encrypted"),
        (&["--compact", "m.aqr"], "mirror"),
        (&["--compact", "one.aqr"], "single file"),
        (&["--compact", "a.aqr", "t0.txt"], "an archive alone"),
    ] {
        let name = args.iter().find(|a| a.ends_with(".aqr")).unwrap();
        let before = fs::read(d.join(name)).unwrap();
        let o = aqr(&d, args);
        assert!(!o.status.success() && stderr(&o).contains(told), "{args:?}: {}", stderr(&o));
        assert_eq!(fs::read(d.join(name)).unwrap(), before, "{args:?}");
    }
    assert!(d.read_dir().unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
    fs::remove_dir_all(&d).unwrap();
}

/// `want` with the files at or under each path `from` moved to `to`, in order.
fn renamed(want: &[(String, Vec<u8>)], moves: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    let mut out = want.to_vec();
    for (from, to) in moves {
        for (p, _) in &mut out {
            if p == from {
                *p = to.to_string();
            } else if let Some(rest) = p.strip_prefix(&format!("{from}/")) {
                *p = format!("{to}/{rest}");
            }
        }
    }
    out.sort();
    out
}

/// Paths moved, a file and a directory, into a directory not there yet and out of a directory
/// into the top; the metadata kept (a file's mode), the archive written again.
#[test]
fn moved() {
    let d = scratch("moved");
    let want = source(&d);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(d.join("src/sub/t1.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    }
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    ok(&d, &["-q", "--delete", "a.aqr", "t3.txt"]);
    let moves = [("sub/deep", "new/place/deep"), ("sub/t1.txt", "t1-moved.txt"), ("t0.txt", "sub/t0.txt")];
    let mut args = vec!["-q", "--move", "a.aqr"];
    for (from, to) in &moves {
        args.extend([*from, "--rename", *to]);
    }
    ok(&d, &args);
    ok(&d, &["-K", "a.aqr"]);
    assert_eq!(extracted(&d, "a.aqr"), renamed(&without(&want, &["t3.txt"]), &moves));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ok(&d, &["-q", "-d", "-o", "x", "a.aqr"]);
        assert_eq!(fs::metadata(d.join("x/t1-moved.txt")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let o = ok(&d, &["-I", "a.aqr"]);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("tombstone"));
    fs::remove_dir_all(&d).unwrap();
}

/// A move to a path there already, under what is not a directory or under itself, from a path
/// not there, or without --rename, is an error, the archive left as it is.
#[test]
fn move_refused() {
    let d = scratch("move-refused");
    source(&d);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    for (args, told) in [
        (&["--move", "a.aqr", "t0.txt", "--rename", "t3.txt"][..], "there already"),
        (&["--move", "a.aqr", "t0.txt", "--rename", "t3.txt/x"], "not a directory"),
        (&["--move", "a.aqr", "sub", "--rename", "sub/deep/sub"], "under itself"),
        (&["--move", "a.aqr", "nope", "--rename", "x"], "no nope in it"),
        (&["--move", "a.aqr", "t0.txt"], "without --rename"),
        (&["--delete", "a.aqr", "t0.txt", "--rename", "x"], "--rename"),
    ] {
        let o = aqr(&d, args);
        assert!(!o.status.success() && stderr(&o).contains(told), "{args:?}: {}", stderr(&o));
        assert_eq!(fs::read(d.join("a.aqr")).unwrap(), a, "{args:?}");
    }
    fs::remove_dir_all(&d).unwrap();
}
