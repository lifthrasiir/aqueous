//! Paths given as the values of options (`-o`, `--rename`, `--stats=`) taken as they are, not
//! as Unicode: a name that is not (bytes not UTF-8 on Unix, a lone surrogate on Windows) names
//! the file it names.

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
