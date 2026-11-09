//! Paths given as the values of options (`-o`, `--rename`, `--stats=`) taken as they are, not
//! as Unicode: a name that is not (bytes not UTF-8 on Unix, a lone surrogate on Windows) names
//! the file it names (where the file system takes such names: not macOS's APFS).

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-args-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// `stem` and a character that is not Unicode, then `ext`.
fn not_unicode(stem: &str, ext: &str) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec([stem.as_bytes(), b"\xff", ext.as_bytes()].concat())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let wide: Vec<u16> = stem.encode_utf16().chain([0xd800]).chain(ext.encode_utf16()).collect();
        OsString::from_wide(&wide)
    }
}

fn aqr(args: &[&std::ffi::OsStr]) {
    let out = Command::new(env!("CARGO_BIN_EXE_aqr")).arg("-q").args(args).output().unwrap();
    assert!(out.status.success(), "aqr {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// The names of the files in `d`, as they are.
fn names(d: &std::path::Path) -> Vec<OsString> {
    let mut n: Vec<OsString> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name()).collect();
    n.sort();
    n
}

#[test]
fn values_not_unicode() {
    let d = scratch("values");
    // (macOS's APFS takes names of UTF-8 alone: nothing to test there)
    if fs::write(d.join(not_unicode("probe", "")), b"").is_err() {
        eprintln!("{}: names not Unicode not taken here, not tested", d.display());
        fs::remove_dir_all(&d).unwrap();
        return;
    }
    fs::remove_file(d.join(not_unicode("probe", ""))).unwrap();
    let input = d.join("in.txt");
    fs::write(&input, "some text\n".repeat(50)).unwrap();
    let (archive, stats, renamed) =
        (d.join(not_unicode("out", ".aqr")), d.join(not_unicode("stats", ".json")), not_unicode("renamed", ""));
    let mut with_stats = OsString::from("--stats=");
    with_stats.push(&stats);
    aqr(&[
        "-c".as_ref(),
        "-o".as_ref(),
        archive.as_os_str(),
        &with_stats,
        input.as_os_str(),
        "--rename".as_ref(),
        &renamed,
    ]);
    assert!(archive.is_file(), "-o: {:?}", names(&d));
    assert!(stats.is_file(), "--stats=: {:?}", names(&d));
    let out = d.join("out");
    aqr(&["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    assert_eq!(names(&out), [renamed]);
}

/// aqr run in `d` on `args`, its exit status and standard error.
fn run_in(d: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).args(args).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn bundled_and_joined() {
    let d = scratch("bundled");
    fs::write(d.join("in.txt"), "some text\n".repeat(50)).unwrap();
    // (short options bundled, the last one's value the next argument or joined)
    for args in [
        &["-qcfo", "a.aqr", "in.txt"][..],
        &["-qcfT2", "-oa.aqr", "-2", "in.txt"],
        &["-q", "--output=a.aqr", "--level=low", "--batch=1M", "--block-size=64K", "--threads=1", "-f", "in.txt"],
        &["-qf", "-B1M", "-l3", "-o", "a.aqr", "--", "in.txt"],
    ] {
        let (ok, err) = run_in(&d, args);
        assert!(ok, "aqr {args:?}: {err}");
        let (ok, err) = run_in(&d, &["-qdfo", "out.txt", "a.aqr"]);
        assert!(ok, "aqr -d of {args:?}: {err}");
        assert_eq!(fs::read(d.join("out.txt")).unwrap(), fs::read(d.join("in.txt")).unwrap());
    }
    for (args, told) in [
        (&["--force=1", "in.txt"][..], "--force takes no value"),
        (&["-qz", "in.txt"], "unknown option -z"),
        (&["--frobnicate", "in.txt"], "unknown option --frobnicate"),
        (&["-qo"], "-o takes a value"),
    ] {
        let (ok, err) = run_in(&d, args);
        assert!(!ok && err.contains(told), "aqr {args:?}: {err}");
    }
    fs::remove_dir_all(&d).unwrap();
}
