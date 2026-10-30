//! The metadata of a directory kept through `aqr -c` and `aqr -d`: times (of nanoseconds, of a
//! read-only file and of a link too), permissions, extended attributes, resource forks, hard
//! links; and what is applied only with `-p` (set-user-ID bits, FIFOs).
#![cfg(unix)]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of its own for test `name`, empty, made writable first where it was left so.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-metadata-{name}-{}", std::process::id()));
    if d.exists() {
        Command::new("chmod").args(["-R", "u+w"]).arg(&d).status().unwrap();
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn aqr(args: &[&std::ffi::OsStr]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_aqr")).args(args).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "aqr {args:?}: {err}");
    err
}

fn c(p: &Path) -> CString {
    CString::new(p.as_os_str().as_bytes()).unwrap()
}

/// Sets the access and modification times of `p` (not followed) to `(secs, nanos)`.
fn set_times(p: &Path, a: (i64, i64), m: (i64, i64)) {
    let t = |(s, n): (i64, i64)| libc::timespec { tv_sec: s as _, tv_nsec: n as _ };
    let times = [t(a), t(m)];
    // SAFETY: two times
    assert_eq!(unsafe { libc::utimensat(libc::AT_FDCWD, c(p).as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) }, 0);
}

fn set_xattr(p: &Path, name: &str, v: &[u8]) {
    let n = CString::new(name).unwrap();
    // SAFETY: `v` is of its length
    #[cfg(target_os = "macos")]
    let r = unsafe { libc::setxattr(c(p).as_ptr(), n.as_ptr(), v.as_ptr().cast(), v.len(), 0, libc::XATTR_NOFOLLOW) };
    // SAFETY: as above
    #[cfg(target_os = "linux")]
    let r = unsafe { libc::lsetxattr(c(p).as_ptr(), n.as_ptr(), v.as_ptr().cast(), v.len(), 0) };
    assert_eq!(r, 0, "{name} of {}: {}", p.display(), std::io::Error::last_os_error());
}

fn xattr(p: &Path, name: &str) -> Option<Vec<u8>> {
    let n = CString::new(name).unwrap();
    let mut b = vec![0u8; 1 << 16];
    // SAFETY: the buffer is of its length
    #[cfg(target_os = "macos")]
    let r =
        unsafe { libc::getxattr(c(p).as_ptr(), n.as_ptr(), b.as_mut_ptr().cast(), b.len(), 0, libc::XATTR_NOFOLLOW) };
    // SAFETY: as above
    #[cfg(target_os = "linux")]
    let r = unsafe { libc::lgetxattr(c(p).as_ptr(), n.as_ptr(), b.as_mut_ptr().cast(), b.len()) };
    (r >= 0).then(|| b[..r as usize].to_vec())
}

fn times(m: &fs::Metadata) -> [(i64, i64); 2] {
    [(m.atime(), m.atime_nsec()), (m.mtime(), m.mtime_nsec())]
}

/// The tree archived: its files and their metadata set as the test checks them.
fn source(d: &Path) -> PathBuf {
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a"), "hello").unwrap();
    fs::hard_link(src.join("a"), src.join("hard")).unwrap();
    fs::write(src.join("ro"), "read only").unwrap();
    fs::write(src.join("suid"), "x").unwrap();
    fs::write(src.join("sub/s"), "s").unwrap();
    std::os::unix::fs::symlink("a", src.join("link")).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c(&src.join("fifo")).as_ptr(), 0o640) }, 0);
    set_xattr(&src.join("a"), "user.aqr", b"value");
    #[cfg(target_os = "macos")]
    fs::write(src.join("a/..namedfork/rsrc"), b"a resource fork").unwrap();
    fs::set_permissions(src.join("suid"), fs::Permissions::from_mode(0o4755)).unwrap();
    // times last, the directories' after what they hold
    set_times(&src.join("a"), (1_262_304_000, 5), (946_684_800, 123_456_789));
    set_times(&src.join("ro"), (1_000_000_000, 0), (1_000_000_001, 1));
    fs::set_permissions(src.join("ro"), fs::Permissions::from_mode(0o444)).unwrap();
    set_times(&src.join("link"), (978_307_200, 7), (978_307_200, 8));
    set_times(&src.join("sub"), (1_012_615_320, 10), (1_012_615_320, 9));
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o555)).unwrap();
    src
}

const CHECKED: [&str; 6] = ["a", "ro", "link", "sub", "sub/s", "hard"];

/// The metadata of the entries checked of `src`, before it is archived (as reading it changes
/// its access times).
fn before(src: &Path) -> Vec<fs::Metadata> {
    CHECKED.iter().map(|p| fs::symlink_metadata(src.join(p)).unwrap()).collect()
}

/// Checks the metadata of `out` against `src` (as `before` was), `privileged` where extracted
/// with `-p`.
fn check(src: &Path, before: &[fs::Metadata], out: &Path, privileged: bool) {
    for (p, s) in CHECKED.iter().zip(before) {
        let o = fs::symlink_metadata(out.join(p)).unwrap();
        assert_eq!(times(s), times(&o), "times of {p}");
        if *p != "link" || cfg!(target_os = "macos") {
            assert_eq!(s.mode(), o.mode(), "mode of {p}");
        }
    }
    let (a, hard) = (fs::metadata(out.join("a")).unwrap(), fs::metadata(out.join("hard")).unwrap());
    assert_eq!((a.ino(), a.nlink()), (hard.ino(), 2), "hard links");
    assert_eq!(xattr(&out.join("a"), "user.aqr").as_deref(), Some(&b"value"[..]));
    #[cfg(not(target_os = "macos"))]
    let _ = src;
    #[cfg(target_os = "macos")]
    {
        assert_eq!(fs::read(out.join("a/..namedfork/rsrc")).unwrap(), b"a resource fork");
        let created = |p: &Path| fs::metadata(p).unwrap().created().unwrap();
        assert_eq!(created(&src.join("a")), created(&out.join("a")));
    }
    assert_eq!(fs::read_link(out.join("link")).unwrap(), Path::new("a"));
    let suid = fs::metadata(out.join("suid")).unwrap().mode() & 0o7777;
    assert_eq!(suid, if privileged { 0o4755 } else { 0o755 });
    let fifo = fs::symlink_metadata(out.join("fifo"));
    assert_eq!(fifo.is_ok(), privileged);
    if privileged {
        use std::os::unix::fs::FileTypeExt;
        assert!(fifo.unwrap().file_type().is_fifo());
    }
}

#[test]
fn metadata_kept() {
    let d = scratch("kept");
    let src = source(&d);
    let was = before(&src);
    let archive = d.join("src.aqr");
    let err = aqr(&["-c".as_ref(), "-o".as_ref(), archive.as_os_str(), src.as_os_str()]);
    assert!(!err.contains("skipped"), "{err}");
    let out = d.join("out");
    let err = aqr(&["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    assert!(err.contains("fifo: a FIFO or a device, not created"), "{err}");
    check(&src, &was, &out, false);
    let out = d.join("out-p");
    aqr(&["-d".as_ref(), "-p".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    check(&src, &was, &out, true);
    Command::new("chmod").args(["-R", "u+w"]).arg(&d).status().unwrap();
    fs::remove_dir_all(&d).unwrap();
}
