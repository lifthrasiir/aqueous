//! Archives of other formats made by `aqr -c` (`--format zip`, or `-o NAME.zip`), extracted by
//! `aqr -d` as they were.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-pack-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn aqr_in(dir: &Path, args: &[&str]) -> Output {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap();
    assert!(o.status.success(), "aqr {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o
}

fn text() -> Vec<u8> {
    (0..1000).flat_map(|i| format!("line {i} of some text\n").into_bytes()).collect()
}

/// The files under `d` by their paths (`/` between names), and their bytes.
fn files(d: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![(d.to_owned(), String::new())];
    while let Some((dir, at)) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = format!("{at}{}", e.file_name().to_string_lossy());
            if e.file_type().unwrap().is_dir() {
                out.push((name.clone() + "/", Vec::new()));
                stack.push((e.path(), name + "/"));
            } else {
                out.push((name, fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn zip_of_a_directory() {
    let d = scratch("dir");
    let src = d.join("src");
    fs::create_dir_all(src.join("sub/deeper")).unwrap();
    fs::write(src.join("a.txt"), text()).unwrap();
    fs::write(src.join("sub/b.bin"), (0..=255u8).cycle().take(5000).collect::<Vec<_>>()).unwrap();
    fs::write(src.join("sub/deeper/한글.txt"), "안녕").unwrap();
    fs::write(src.join("empty"), "").unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_700);
    fs::File::options().write(true).open(src.join("a.txt")).unwrap().set_modified(t).unwrap();
    aqr_in(&d, &["-q", "--format", "zip", "src"]);
    let zip = fs::read(d.join("src.zip")).unwrap();
    assert_eq!(&zip[..4], b"PK\x03\x04");
    // (the directory itself in it, by its name)
    fs::create_dir(d.join("out")).unwrap();
    aqr_in(&d, &["-q", "-d", "-o", "out", "src.zip"]);
    assert_eq!(files(&d.join("out/src")), files(&src));
    assert_eq!(fs::metadata(d.join("out/src/a.txt")).unwrap().modified().unwrap(), t);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn zip_by_its_name() {
    let d = scratch("named");
    fs::create_dir(d.join("in")).unwrap();
    fs::write(d.join("in/one.txt"), text()).unwrap();
    fs::write(d.join("two.txt"), "two").unwrap();
    // (several inputs by their paths; a single file at its name alone)
    aqr_in(&d, &["-q", "-o", "both.zip", "in/one.txt", "two.txt"]);
    aqr_in(&d, &["-q", "--format", "zip", "-0", "in/one.txt"]);
    fs::create_dir(d.join("x")).unwrap();
    aqr_in(&d, &["-q", "-d", "-o", "x", "both.zip"]);
    let want = vec![("in/".into(), vec![]), ("in/one.txt".into(), text()), ("two.txt".into(), b"two".to_vec())];
    assert_eq!(files(&d.join("x")), want);
    let stored = fs::read(d.join("in/one.txt.zip")).unwrap();
    // (stored: the text there as it is)
    assert!(stored.windows(20).any(|w| w == &text()[..20]));
    fs::create_dir(d.join("y")).unwrap();
    aqr_in(&d, &["-q", "-d", "-o", "y", "in/one.txt.zip"]);
    assert_eq!(files(&d.join("y")), vec![("one.txt".into(), text())]);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn zip_refuses_a_passphrase() {
    let d = scratch("pass");
    fs::write(d.join("f"), "f").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(&d).args(["-P", "-o", "f.zip", "f"]).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("not encrypted yet"));
    fs::remove_dir_all(&d).unwrap();
}
