//! The metadata of a directory kept through `aqr -c` and `aqr -d` on Windows: times (the
//! creation time too, of a read-only file as well), file attributes, alternate data streams and
//! hard links.
#![cfg(windows)]

use std::fs;
use std::os::windows::fs::{FileTimesExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HIDDEN: u32 = 0x2;
const READONLY: u32 = 0x1;
/// `FILE_FLAG_BACKUP_SEMANTICS`, to open directories.
const BACKUP: u32 = 0x0200_0000;
/// `FILE_WRITE_ATTRIBUTES`.
const WRITE_ATTRIBUTES: u32 = 0x100;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-metadata-{name}-{}", std::process::id()));
    if d.exists() {
        unreadonly(&d);
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// Clears the read-only attribute of everything under `d`, so that it can be removed.
fn unreadonly(d: &Path) {
    for e in fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
        if p.is_dir() {
            unreadonly(&p);
        }
    }
}

fn aqr(args: &[&std::ffi::OsStr]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_aqr")).args(args).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "aqr {args:?}: {err}");
    err
}

/// The path of stream `name` of `p` (not joined, as `a:x` would be a drive's).
fn stream(p: &Path, name: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(":");
    s.push(name);
    s.into()
}

fn at(secs: u64, hundreds: u32) -> SystemTime {
    UNIX_EPOCH + Duration::new(secs, hundreds * 100)
}

/// Sets the creation, access and modification times of `p`.
fn set_times(p: &Path, c: SystemTime, a: SystemTime, m: SystemTime) {
    let f = fs::OpenOptions::new().access_mode(WRITE_ATTRIBUTES).custom_flags(BACKUP).open(p).unwrap();
    f.set_times(fs::FileTimes::new().set_created(c).set_accessed(a).set_modified(m)).unwrap();
}

fn source(d: &Path) -> PathBuf {
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a"), "hello").unwrap();
    fs::write(stream(&src.join("a"), "Zone.Identifier"), "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
    fs::write(stream(&src.join("sub"), "on a directory"), "a stream of a directory").unwrap();
    fs::hard_link(src.join("a"), src.join("hard")).unwrap();
    fs::write(src.join("ro"), "read only").unwrap();
    fs::write(src.join("sub/s"), "s").unwrap();
    set_times(&src.join("a"), at(946_684_800, 1), at(1_262_304_000, 2), at(978_307_200, 3));
    set_times(&src.join("ro"), at(1_000_000_000, 4), at(1_000_000_001, 5), at(1_000_000_002, 6));
    let mut perm = fs::metadata(src.join("ro")).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(src.join("ro"), perm).unwrap();
    let hidden = src.join("a").as_os_str().encode_wide_with_nul();
    // SAFETY: a path of a NUL
    assert_ne!(unsafe { SetFileAttributesW(hidden.as_ptr(), HIDDEN) }, 0);
    set_times(&src.join("sub"), at(1_012_615_320, 7), at(1_012_615_320, 8), at(1_012_615_320, 9));
    src
}

trait Wide {
    fn encode_wide_with_nul(&self) -> Vec<u16>;
}

impl Wide for std::ffi::OsStr {
    fn encode_wide_with_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetFileAttributesW(name: *const u16, attrs: u32) -> i32;
}

const CHECKED: [&str; 5] = ["a", "hard", "ro", "sub", "sub/s"];

/// The times and attributes of the entries checked (before they are read).
fn snapshot(root: &Path) -> Vec<(u64, u64, u64, u32)> {
    CHECKED
        .iter()
        .map(|p| {
            let m = fs::symlink_metadata(root.join(p)).unwrap();
            (m.creation_time(), m.last_access_time(), m.last_write_time(), m.file_attributes() & 0x2127)
        })
        .collect()
}

#[test]
fn metadata_kept() {
    let d = scratch("kept");
    let src = source(&d);
    let was = snapshot(&src);
    let archive = d.join("src.aqr");
    let err = aqr(&["-c".as_ref(), "-o".as_ref(), archive.as_os_str(), src.as_os_str()]);
    assert!(!err.contains("not read"), "{err}");
    let out = d.join("out");
    let err = aqr(&["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    assert!(!err.contains("not written"), "{err}");
    for ((p, w), h) in CHECKED.iter().zip(&was).zip(snapshot(&out)) {
        assert_eq!(*w, h, "times and attributes of {p}");
    }
    let zone = fs::read_to_string(stream(&out.join("a"), "Zone.Identifier")).unwrap();
    assert_eq!(zone, "[ZoneTransfer]\r\nZoneId=3\r\n");
    assert_eq!(fs::read_to_string(stream(&out.join("sub"), "on a directory")).unwrap(), "a stream of a directory");
    // a hard link: a change to one seen in the other (written over, as a hidden file cannot be
    // created again)
    use std::io::Write;
    fs::OpenOptions::new().write(true).open(out.join("a")).unwrap().write_all(b"changed").unwrap();
    assert_eq!(fs::read_to_string(out.join("hard")).unwrap(), "changed");
    unreadonly(&d);
    fs::remove_dir_all(&d).unwrap();
}
