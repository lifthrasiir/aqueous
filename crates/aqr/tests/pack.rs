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

/// The files under `d` by their paths (`/` between names), and their bytes (by their length and
/// a hash of them, so that a failure tells briefly).
fn files(d: &Path) -> Vec<(String, (usize, u64))> {
    let of = |b: Vec<u8>| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        b.hash(&mut h);
        (b.len(), h.finish())
    };
    files_raw(d).into_iter().map(|(n, b)| (n, of(b))).collect()
}

fn files_raw(d: &Path) -> Vec<(String, Vec<u8>)> {
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
    assert_eq!(files_raw(&d.join("x")), want);
    let stored = fs::read(d.join("in/one.txt.zip")).unwrap();
    // (stored: the text there as it is)
    assert!(stored.windows(20).any(|w| w == &text()[..20]));
    fs::create_dir(d.join("y")).unwrap();
    aqr_in(&d, &["-q", "-d", "-o", "y", "in/one.txt.zip"]);
    assert_eq!(files_raw(&d.join("y")), vec![("one.txt".into(), text())]);
    fs::remove_dir_all(&d).unwrap();
}

/// `aqr` given `args` in `dir`, standard input `input`.
fn aqr_fed(dir: &Path, args: &[&str], input: &[u8]) -> Output {
    use std::io::Write;
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input).unwrap();
    c.wait_with_output().unwrap()
}

#[test]
fn zip_encrypted() {
    let d = scratch("pass");
    fs::write(d.join("f"), text()).unwrap();
    let o = aqr_fed(&d, &["-q", "-P", "-o", "f.zip", "f"], b"secret\n");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let zip = fs::read(d.join("f.zip")).unwrap();
    assert!(!zip.windows(20).any(|w| w == &text()[..20]));
    fs::create_dir(d.join("x")).unwrap();
    let o = aqr_fed(&d, &["-q", "-d", "-P", "-o", "x", "f.zip"], b"secret\n");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(files_raw(&d.join("x")), vec![("f".into(), text())]);
    // (a format of no encryption refuses it)
    let o = aqr_fed(&d, &["-P", "-o", "f.tar.gz", "f"], b"secret\n");
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("not encrypted"));
    fs::remove_dir_all(&d).unwrap();
}

/// A directory to archive: text, bytes, an empty file, a nested Korean name, a symbolic link.
fn tree_in(src: &Path) {
    fs::create_dir_all(src.join("sub/deeper")).unwrap();
    fs::write(src.join("a.txt"), text().repeat(30)).unwrap();
    fs::write(src.join("sub/b.bin"), (0..=255u8).cycle().take(50000).collect::<Vec<_>>()).unwrap();
    fs::write(src.join("sub/deeper/한글.txt"), "안녕").unwrap();
    fs::write(src.join("empty"), "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
}

/// A black box `tool args file`, where it is at hand: whether it was.
fn checked_by(tool: &str, args: &[&str], file: &Path) -> bool {
    let Ok(o) = Command::new(tool).args(args).arg(file).output() else { return false };
    assert!(o.status.success(), "{tool} {args:?} {}: {}", file.display(), String::from_utf8_lossy(&o.stderr));
    true
}

#[test]
fn tar_under_each_compressor() {
    let d = scratch("tar");
    let src = d.join("src");
    tree_in(&src);
    let t = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_700);
    fs::File::options().write(true).open(src.join("a.txt")).unwrap().set_modified(t).unwrap();
    for (ext, check) in [
        ("tar", None),
        ("tar.gz", Some(("gzip", "-t"))),
        ("tar.bz2", Some(("bzip2", "-t"))),
        ("tar.xz", Some(("xz", "-t"))),
        ("tar.lzma", Some(("xz", "-t"))),
        ("tar.zst", Some(("zstd", "-t"))),
    ] {
        // (on 4 threads, parts small: a few of them)
        aqr_in(&d, &["-q", "-T", "4", "--format", ext, "src"]);
        let a = d.join(format!("src.{ext}"));
        if let Some((tool, flag)) = check {
            checked_by(tool, &[flag], &a);
        }
        if cfg!(unix) && Command::new("tar").arg("--version").output().is_ok() {
            let x = d.join(format!("by-tar-{ext}"));
            fs::create_dir(&x).unwrap();
            let o = Command::new("tar").arg("-xf").arg(&a).arg("-C").arg(&x).output().unwrap();
            assert!(o.status.success(), "tar -xf {ext}: {}", String::from_utf8_lossy(&o.stderr));
            // (by the paths of the files, as macOS may have the names in another normal form)
            for (name, want) in files(&src).into_iter().filter(|f| !f.0.ends_with('/')) {
                let got = fs::read(x.join("src").join(&name)).unwrap();
                assert_eq!(got.len(), want.0, "{ext} by tar: {name}");
            }
        }
        let x = d.join(format!("x-{ext}"));
        fs::create_dir(&x).unwrap();
        aqr_in(&d, &["-q", "-d", "-o", x.to_str().unwrap(), a.to_str().unwrap()]);
        assert_eq!(files(&x.join("src")), files(&src), "{ext}");
        assert_eq!(fs::metadata(x.join("src/a.txt")).unwrap().modified().unwrap(), t, "{ext}");
        #[cfg(unix)]
        assert_eq!(fs::read_link(x.join("src/link")).unwrap(), Path::new("a.txt"), "{ext}");
    }
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn single_files() {
    let d = scratch("single");
    // (large enough for several parts on 4 threads: 1 MiB each, at least)
    let big: Vec<u8> = text().repeat(150);
    fs::write(d.join("f.txt"), &big).unwrap();
    for (ext, tool) in [("gz", "gzip"), ("bz2", "bzip2"), ("xz", "xz"), ("lzma", "xz"), ("zst", "zstd")] {
        aqr_in(&d, &["-q", "-T", "4", "--format", ext, "f.txt"]);
        let a = d.join(format!("f.txt.{ext}"));
        assert!(fs::metadata(&a).unwrap().len() < big.len() as u64 / 4, "{ext}");
        if checked_by(tool, &["-t"], &a) {
            let o = Command::new(tool).arg("-dc").arg(&a).output().unwrap();
            assert!(o.stdout == big, "{ext} by {tool}");
        }
        let x = d.join(format!("x-{ext}"));
        aqr_in(&d, &["-q", "-d", "-o", x.to_str().unwrap(), a.to_str().unwrap()]);
        assert!(fs::read(&x).unwrap() == big, "{ext}");
    }
    // (a directory is not a single file; levels are the format's)
    fs::create_dir(d.join("dir")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(&d).args(["-o", "dir.gz", "dir"]).output().unwrap();
    assert!(!o.status.success());
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(&d).args(["-l", "0", "-o", "f.bz2", "f.txt"]).output();
    assert!(!o.unwrap().status.success());
    aqr_in(&d, &["-q", "-f", "-l", "19", "-o", "f.zst", "f.txt"]);
    fs::remove_dir_all(&d).unwrap();
}

/// The header blocks of the tar `a` (not compressed), by name: the type flag of each.
#[cfg(unix)]
fn tar_types(a: &[u8]) -> Vec<(String, u8)> {
    let (mut out, mut at) = (Vec::new(), 0);
    while at + 512 <= a.len() && a[at] != 0 {
        let h = &a[at..at + 512];
        let name = String::from_utf8_lossy(&h[..100]).trim_end_matches('\0').to_owned();
        let size = u64::from_str_radix(std::str::from_utf8(&h[124..136]).unwrap().trim_matches(['\0', ' ']), 8);
        out.push((name, h[156]));
        at += 512 + (size.unwrap() as usize).div_ceil(512) * 512;
    }
    out
}

#[cfg(unix)]
#[test]
fn tar_keeps_hard_links() {
    use std::os::unix::fs::MetadataExt;
    let d = scratch("tar-hard");
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), text()).unwrap();
    fs::hard_link(src.join("a.txt"), src.join("sub/hard")).unwrap();
    aqr_in(&d, &["-q", "-o", "src.tar", "src"]);
    let types = tar_types(&fs::read(d.join("src.tar")).unwrap());
    let of = |n: &str| types.iter().find(|t| t.0 == n).unwrap_or_else(|| panic!("{n} in {types:?}")).1;
    assert_eq!((of("src/a.txt"), of("src/sub/hard")), (b'0', b'1'), "{types:?}");
    let x = d.join("x");
    fs::create_dir(&x).unwrap();
    aqr_in(&d, &["-q", "-d", "-o", "x", "src.tar"]);
    let (a, h) = (fs::metadata(x.join("src/a.txt")).unwrap(), fs::metadata(x.join("src/sub/hard")).unwrap());
    assert_eq!((a.ino(), a.nlink()), (h.ino(), 2), "hard links");
    assert_eq!(fs::read(x.join("src/sub/hard")).unwrap(), text());
    fs::remove_dir_all(&d).unwrap();
}
