//! Archives of other formats extracted by `aqr -d` (and tested by `-t`), named by their
//! formats: the files made with their metadata, what is left out told, and an error at the end
//! for it (aqr-unpack's fixtures).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, UNIX_EPOCH};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-unpack-{name}-{}", std::process::id()));
    if d.exists() {
        unlock(&d);
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// Makes everything under `d` writable, so that it can be removed.
fn unlock(d: &Path) {
    for e in fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        let m = fs::symlink_metadata(&p).unwrap();
        if m.is_symlink() {
            continue;
        }
        let mut perm = m.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
        if m.is_dir() {
            unlock(&p);
        }
    }
}

/// A fixture of aqr-unpack copied into `d` as `name`.
fn fixture(d: &Path, fixture: &str, name: &str) -> PathBuf {
    let to = d.join(name);
    fs::copy(format!("{}/../aqr-unpack/tests/data/{fixture}", env!("CARGO_MANIFEST_DIR")), &to).unwrap();
    to
}

fn aqr(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).args(args).output().unwrap()
}

fn text() -> Vec<u8> {
    (0..1000).flat_map(|i| format!("line {i} of some text\n").into_bytes()).collect()
}

#[test]
fn tar_extracted() {
    let d = scratch("tar");
    let archive = fixture(&d, "tree.tar.gz", "tree.tar.gz");
    let o = aqr(&["-d".as_ref(), archive.as_os_str()]);
    let err = String::from_utf8_lossy(&o.stderr);
    // `../evil.txt` left out and told, and the run failing for it, once all else is done
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("../evil.txt: a path reaching out") && err.contains("1 entry not extracted"), "{err}");
    let out = d.join("tree");
    assert!(!d.join("evil.txt").exists());
    assert_eq!(fs::read(out.join("dir/file.txt")).unwrap(), text());
    let m = fs::metadata(out.join("dir/file.txt")).unwrap();
    assert_eq!(m.modified().unwrap(), UNIX_EPOCH + Duration::new(1_500_000_001, 250_000_000));
    assert_eq!(
        fs::metadata(out.join("dir")).unwrap().modified().unwrap(),
        UNIX_EPOCH + Duration::from_secs(1_400_000_000)
    );
    assert!(fs::metadata(out.join("dir/ro.txt")).unwrap().permissions().readonly());
    assert_eq!(fs::read(out.join("dup.txt")).unwrap(), b"second");
    assert_eq!(fs::read(out.join("abs.txt")).unwrap(), b"absolute");
    let long = format!("long/{}/{}.txt", "n".repeat(150), "x".repeat(120));
    assert_eq!(fs::read(out.join(long)).unwrap(), b"long name");
    assert_eq!(fs::read(out.join("한글/파일.txt")).unwrap(), "유니코드".as_bytes());
    // a hard link: a change to one seen in the other
    fs::write(out.join("dir/hard"), "changed").unwrap();
    assert_eq!(fs::read(out.join("dir/file.txt")).unwrap(), b"changed");
    assert_eq!(fs::read_link(out.join("dir/sym")).unwrap(), Path::new("file.txt"));
    assert!(err.contains("a link reaching out of") && fs::symlink_metadata(out.join("dir/escape")).is_err(), "{err}");
    assert!(err.contains("fifo: a FIFO or a device, not created"), "{err}");
    if cfg!(windows) {
        assert!(err.contains("written as 12_30.txt"), "{err}");
        assert_eq!(fs::read(out.join("12_30.txt")).unwrap(), b"colon");
    } else {
        assert_eq!(fs::read(out.join("12:30.txt")).unwrap(), b"colon");
    }
    // there already: not written over without -f
    let o = aqr(&["-d".as_ref(), archive.as_os_str()]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("exists (-f to overwrite)"));
    unlock(&d);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn zip_extracted() {
    let d = scratch("zip");
    let archive = fixture(&d, "tree.zip", "tree.zip");
    let o = aqr(&["-d".as_ref(), "-o".as_ref(), d.join("out").as_os_str(), archive.as_os_str()]);
    assert!(!o.status.success());
    let out = d.join("out");
    for f in ["stored.txt", "deflated.txt", "bzip2.txt", "lzma.txt"] {
        assert_eq!(fs::read(out.join(f)).unwrap(), text(), "{f}");
    }
    assert_eq!(fs::read(out.join("dir/in.txt")).unwrap(), b"in dir");
    assert_eq!(fs::read_link(out.join("link")).unwrap(), Path::new("stored.txt"));
    // tested: the same left out, nothing written
    let o = aqr(&["-t".as_ref(), archive.as_os_str()]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("1 entry not extracted"), "{err}");
    assert!(!d.join("tree").exists());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn single_files() {
    let d = scratch("single");
    for (fixture_name, name) in [("one.txt.xz", "a.txt.xz"), ("one.txt.gz", "b.txt.gz"), ("one.txt.bz2", "c.txt.bz2")] {
        let archive = fixture(&d, fixture_name, name);
        let o = aqr(&["-t".as_ref(), archive.as_os_str()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = aqr(&["-d".as_ref(), archive.as_os_str()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(fs::read(d.join(&name[..5])).unwrap(), text(), "{name}");
    }
    // damaged: an error, and nothing left of the output
    let mut x = fs::read(d.join("a.txt.xz")).unwrap();
    x[100] ^= 1;
    fs::write(d.join("bad.txt.xz"), x).unwrap();
    let o = aqr(&["-d".as_ref(), d.join("bad.txt.xz").as_os_str()]);
    assert!(!o.status.success());
    assert!(!d.join("bad.txt").exists());
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
    fs::remove_dir_all(&d).unwrap();
}
