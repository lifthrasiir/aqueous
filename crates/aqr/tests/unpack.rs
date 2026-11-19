//! Archives of other formats extracted by `aqr -d` (and tested by `-K`), named by their
//! formats: the files made with their metadata, what is left out told, and an error at the end
//! for it (aqr-unpack's fixtures); extracted as they are into the current directory (or `-o`),
//! and with `--auto-dir` a single entry at their top beside them, else a directory of their
//! name.

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

/// `aqr` with `args`, run in directory `dir`.
fn aqr_in(dir: &Path, args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap()
}

fn text() -> Vec<u8> {
    (0..1000).flat_map(|i| format!("line {i} of some text\n").into_bytes()).collect()
}

#[test]
fn tar_extracted() {
    let d = scratch("tar");
    let archive = fixture(&d, "tree.tar.gz", "tree.tar.gz");
    let out = d.join("tree");
    let o = aqr(&["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    let err = String::from_utf8_lossy(&o.stderr);
    // `../evil.txt` left out and told, and the run failing for it, once all else is done
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("../evil.txt: a path reaching out") && err.contains("1 entry not extracted"), "{err}");
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
    let o = aqr(&["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
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
    let o = aqr(&["-K".as_ref(), archive.as_os_str()]);
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
        let o = aqr(&["-K".as_ref(), archive.as_os_str()]);
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

/// A tar archive (ustar) of `members`: paths, and contents (`None`: a directory).
fn tar(members: &[(String, Option<&[u8]>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (path, data) in members {
        let mut h = [0u8; 512];
        h[..path.len()].copy_from_slice(path.as_bytes());
        let size = data.map_or(0, |d| d.len());
        let mode: &[u8] = if data.is_some() { b"0000644\0" } else { b"0000755\0" };
        h[100..108].copy_from_slice(mode);
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(format!("{:011o}\0", 1_600_000_000).as_bytes());
        h[156] = if data.is_some() { b'0' } else { b'5' };
        h[257..265].copy_from_slice(b"ustar\x0000");
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        if let Some(d) = data {
            out.extend_from_slice(d);
            out.resize(out.len().next_multiple_of(512), 0);
        }
    }
    out.resize(out.len() + 1024, 0);
    out
}

#[test]
fn extracted_as_is() {
    let d = scratch("as-is");
    // into the current directory, a single directory at the top too (of the archive's name or
    // not), beside what is there
    fs::write(d.join("other.txt"), b"other").unwrap();
    for name in ["wrapped.zip", "proj.zip"] {
        let archive = fixture(&d, "wrapped.zip", name);
        let o = aqr_in(&d, &["-d".as_ref(), "-f".as_ref(), archive.as_os_str()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(fs::read(d.join("proj/a.txt")).unwrap(), b"a", "{name}");
        assert_eq!(fs::read(d.join("proj/sub/b.txt")).unwrap(), b"b", "{name}");
        assert!(!d.join("wrapped").exists() && !d.join("proj/proj").exists(), "{name}");
        // (`../x` from `proj/` is in the directory extracted into)
        assert_eq!(fs::read_link(d.join("proj/out")).unwrap(), Path::new("../x"), "{name}");
    }
    assert_eq!(fs::read(d.join("other.txt")).unwrap(), b"other");
    // a file there already: not written over without -f
    let o = aqr_in(&d, &["-d".as_ref(), d.join("proj.zip").as_os_str()]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("exists (-f to overwrite)"), "{err}");
    // into -o, made where it is not
    let o = aqr(&["-d".as_ref(), "-o".as_ref(), d.join("x/y").as_os_str(), d.join("proj.zip").as_os_str()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(d.join("x/y/proj/a.txt")).unwrap(), b"a");
    unlock(&d);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn auto_dir() {
    let d = scratch("auto");
    let auto = |args: &[&std::ffi::OsStr]| {
        let o = aqr_in(&d, &[&["-d".as_ref(), "--auto-dir".as_ref()][..], args].concat());
        (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    // a single directory at the top, of another name: beside the archive as it is (run
    // elsewhere), its time kept
    fs::create_dir(d.join("in")).unwrap();
    let archive = fixture(&d.join("in"), "wrapped.zip", "wrapped.zip");
    let (ok, err) = auto(&[archive.as_os_str()]);
    assert!(ok, "{err}");
    let proj = d.join("in/proj");
    assert_eq!(fs::read(proj.join("a.txt")).unwrap(), b"a");
    assert_eq!(fs::read(proj.join("sub/b.txt")).unwrap(), b"b");
    assert_eq!(fs::read_link(proj.join("link")).unwrap(), Path::new("a.txt"));
    // (`../x` from `proj/` reaches out of what is extracted)
    assert!(err.contains("a link reaching out of") && fs::symlink_metadata(proj.join("out")).is_err(), "{err}");
    assert!(!d.join("in/wrapped").exists() && !d.join("proj").exists());
    // (2019-05-06 07:08:10 local time: within 14 hours of it in UTC)
    let t = fs::metadata(&proj).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs();
    assert!(t.abs_diff(1_557_126_490) <= 14 * 3600, "{t}");
    // `proj` there already: into a directory of the archive's name; that there too, an error
    let (ok, err) = auto(&[archive.as_os_str()]);
    assert!(ok, "{err}");
    assert_eq!(fs::read(d.join("in/wrapped/proj/a.txt")).unwrap(), b"a");
    let (ok, err) = auto(&[archive.as_os_str()]);
    assert!(!ok && err.contains("exists (-f to overwrite)"), "{err}");
    // of the archive's name (case aside where the system takes it so), there already: written
    // over with -f, not into `proj/proj`
    let name = if cfg!(any(windows, target_os = "macos")) { "PROJ.zip" } else { "proj.zip" };
    let archive = fixture(&d.join("in"), "wrapped.zip", name);
    let (ok, err) = auto(&[archive.as_os_str()]);
    assert!(!ok && err.contains("exists (-f to overwrite)"), "{err}");
    let (ok, err) = auto(&["-f".as_ref(), archive.as_os_str()]);
    assert!(ok, "{err}");
    assert!(!proj.join("proj").exists() && !d.join("in/PROJ/proj").exists());
    // several entries at the top: into a directory of the archive's name, in -o
    let archive = fixture(&d, "tree.zip", "tree.zip");
    let (ok, err) = auto(&["-o".as_ref(), "out".as_ref(), archive.as_os_str()]);
    assert!(!ok && err.contains("1 entry not extracted"), "{err}");
    assert_eq!(fs::read(d.join("out/tree/stored.txt")).unwrap(), text());
    assert_eq!(fs::read(d.join("out/tree/dir/in.txt")).unwrap(), b"in dir");
    // a single file at the top: beside the archive
    fs::write(d.join("one.tar"), tar(&[("one.txt".into(), Some(b"one"))])).unwrap();
    let (ok, err) = auto(&["one.tar".as_ref()]);
    assert!(ok, "{err}");
    assert_eq!(fs::read(d.join("one.txt")).unwrap(), b"one");
    assert!(!d.join("one").exists());
    // a tar archive read as it is decoded, of more members than a batch: `proj/` beside it,
    // then (another entry at the top after the first batch) moved into a directory of its name
    let files: Vec<(String, Option<&[u8]>)> = std::iter::once(("proj/".to_string(), None))
        .chain((0..4200).map(|i| (format!("proj/f{i}"), Some(&b"x"[..]))))
        .collect();
    let late: Vec<_> = files.iter().cloned().chain([("late.txt".to_string(), Some(&b"late"[..]))]).collect();
    for (dir, members) in [("single", &files), ("late", &late)] {
        fs::create_dir(d.join(dir)).unwrap();
        fs::write(d.join(dir).join("stuff.tar"), tar(members)).unwrap();
        let (ok, err) = auto(&[d.join(dir).join("stuff.tar").as_os_str()]);
        assert!(ok, "{err}");
    }
    assert_eq!(fs::read(d.join("single/proj/f0")).unwrap(), b"x");
    assert_eq!(fs::read(d.join("single/proj/f4199")).unwrap(), b"x");
    assert!(!d.join("single/stuff").exists());
    assert_eq!(fs::read(d.join("late/stuff/proj/f0")).unwrap(), b"x");
    assert_eq!(fs::read(d.join("late/stuff/proj/f4199")).unwrap(), b"x");
    assert_eq!(fs::read(d.join("late/stuff/late.txt")).unwrap(), b"late");
    assert!(!d.join("late/proj").exists());
    let t = fs::metadata(d.join("late/stuff/proj")).unwrap().modified().unwrap();
    assert_eq!(t, UNIX_EPOCH + Duration::from_secs(1_600_000_000));
    // an Aqueous archive of a directory holding a single one: that beside it
    fs::create_dir_all(d.join("src/inner")).unwrap();
    fs::write(d.join("src/inner/f"), b"f").unwrap();
    assert!(aqr(&["-q".as_ref(), d.join("src").as_os_str()]).status.success());
    fs::rename(d.join("src.aqr"), d.join("in/src.aqr")).unwrap();
    let (ok, err) = auto(&[d.join("in/src.aqr").as_os_str()]);
    assert!(ok, "{err}");
    assert_eq!(fs::read(d.join("in/inner/f")).unwrap(), b"f");
    assert!(!d.join("in/src").exists());
    unlock(&d);
    fs::remove_dir_all(&d).unwrap();
}

/// A pax header of `records` (keys and values) for the member after it.
#[cfg(unix)]
fn pax(records: &[(&str, &[u8])]) -> Vec<u8> {
    let mut d = Vec::new();
    for (k, v) in records {
        // (the length counts itself)
        let rest = 1 + k.len() + 1 + v.len() + 1;
        let mut n = rest + 1;
        while format!("{n}").len() + rest != n {
            n += 1;
        }
        d.extend_from_slice(format!("{n} {k}=").as_bytes());
        d.extend_from_slice(v);
        d.push(b'\n');
    }
    let mut out = tar(&[("PaxHeaders/x".into(), Some(&d))]);
    out.truncate(out.len() - 1024);
    out[156] = b'x';
    out[148..156].fill(b' ');
    let sum: u32 = out[..512].iter().map(|&b| b as u32).sum();
    out[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out
}

/// The extended attributes of a tar's member applied without `-p` only where they are of the
/// users' namespace: one that may give privileges (`security.capability` on Linux) is not
/// written, told once for all the entries having it.
#[cfg(unix)]
#[test]
fn privileged_xattrs_held() {
    let d = scratch("xattrs");
    let held = if cfg!(target_os = "macos") { "com.apple.system.aqr" } else { "security.capability" };
    let records: [(&str, &[u8]); 2] =
        [(&format!("SCHILY.xattr.{held}"), b"\x01\x00\x00\x02"), ("SCHILY.xattr.user.aqr", b"kept")];
    let mut t = Vec::new();
    for name in ["a", "b"] {
        t.extend(pax(&records));
        let mut m = tar(&[(name.into(), Some(b"x"))]);
        m.truncate(m.len() - 1024);
        t.extend(m);
    }
    t.resize(t.len() + 1024, 0);
    let archive = d.join("x.tar");
    fs::write(&archive, t).unwrap();
    let out = d.join("out");
    let o = aqr(&["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    let told = format!("extended attribute {held} not written, nor of any other entry (-p to write it)");
    assert_eq!(err.matches(&told).count(), 1, "{err}");
    let xattr = |p: &Path, name: &str| {
        let c = |s: &[u8]| std::ffi::CString::new(s).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let (p, n) = (c(p.as_os_str().as_bytes()), c(name.as_bytes()));
        let mut b = [0u8; 64];
        // SAFETY: the buffer is of its length
        #[cfg(target_os = "macos")]
        let r = unsafe { libc::getxattr(p.as_ptr(), n.as_ptr(), b.as_mut_ptr().cast(), b.len(), 0, 0) };
        // SAFETY: as above
        #[cfg(not(target_os = "macos"))]
        let r = unsafe { libc::getxattr(p.as_ptr(), n.as_ptr(), b.as_mut_ptr().cast(), b.len()) };
        (r >= 0).then(|| b[..r as usize].to_vec())
    };
    for name in ["a", "b"] {
        assert_eq!(xattr(&out.join(name), "user.aqr").as_deref(), Some(&b"kept"[..]), "{name}");
        assert_eq!(xattr(&out.join(name), held), None, "{name}");
    }
    fs::remove_dir_all(&d).unwrap();
}

/// Encrypted entries of other formats: extracted with the passphrase, left out (told, an error
/// at the end) with none; a 7z of encrypted headers, and a wrong passphrase, refused whole.
#[test]
fn encrypted_extracted() {
    let d = scratch("enc");
    fs::write(d.join("pass"), "pass word\n").unwrap();
    fs::write(d.join("pass7"), "päss wörd\n").unwrap();
    fs::write(d.join("wrong"), "nope\n").unwrap();
    let run = |args: &[&str]| {
        let args: Vec<&std::ffi::OsStr> = args.iter().map(|s| s.as_ref()).collect();
        aqr_in(&d, &args)
    };
    for (name, pass) in [("enc-aes256.zip", "pass"), ("enc-zipcrypto.zip", "pass"), ("enc.7z", "pass7")] {
        fixture(&d, name, name);
        let out = format!("out-{name}");
        let pf = format!("--passphrase-from={pass}");
        let o = run(&["-d", &pf, "-o", &out, name]);
        assert!(o.status.success(), "{name}: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(fs::read(d.join(&out).join("secret.txt")).unwrap(), text(), "{name}");
        assert_eq!(fs::read(d.join(&out).join("small.txt")).unwrap(), b"small\n", "{name}");
        // tested alike
        let o = run(&["-K", &pf, name]);
        assert!(o.status.success(), "{name}: {}", String::from_utf8_lossy(&o.stderr));
        // a wrong passphrase leaves them out
        let o = run(&["-d", "--passphrase-from=wrong", "-o", &format!("w-{name}"), name]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains("encrypted"), "{name}: {err}");
    }
    // the headers encrypted too: nothing is known without the passphrase
    fixture(&d, "enc-headers.7z", "h.7z");
    let o = run(&["-d", "--passphrase-from=wrong", "-o", "h-out", "h.7z"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("passphrase"), "{err}");
    assert!(!d.join("h-out").join("secret.txt").exists());
    fs::remove_dir_all(&d).unwrap();
}

/// `--stats=FILE.json` of an archive of another format extracted: its run, with the contents.
#[test]
fn foreign_stats() {
    let d = scratch("stats");
    fixture(&d, "tree.tar.gz", "t.tar.gz");
    fixture(&d, "one.txt.xz", "one.txt.xz");
    let o = aqr_in(&d, &["-d".as_ref(), "--stats=s.json".as_ref(), "-o".as_ref(), "out".as_ref(), "t.tar.gz".as_ref()]);
    // (an entry left out is an error at the end, the stats written all the same)
    assert!(!o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&fs::read(d.join("s.json")).unwrap()).unwrap();
    let run = &v["runs"][0];
    assert_eq!(run["mode"], "decompress");
    assert_eq!(run["input"], "t.tar.gz");
    assert!(run["written"].as_u64().unwrap() > 0);
    assert!(
        run["contents"].as_array().unwrap().iter().any(|c| c["path"].as_str().is_some_and(|p| p.ends_with("file.txt")))
    );
    // a single stream, tested: one content, of the length decoded
    let o = aqr_in(&d, &["-K".as_ref(), "--stats=u.json".as_ref(), "one.txt.xz".as_ref()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&fs::read(d.join("u.json")).unwrap()).unwrap();
    assert_eq!(v["runs"][0]["mode"], "test");
    assert_eq!(v["runs"][0]["contents"][0]["len"], text().len());
    fs::remove_dir_all(&d).unwrap();
}
