//! An archive mounted through FUSE, as the program is run (the `aqr` built beside the tests:
//! `cargo rt -p aqr -p aqr-mount-fuse`): its files read whole, writes refused, SIGTERM
//! unmounting. Skipped where FUSE is not to be had (no `/dev/fuse`, no `fusermount3`: a
//! container).
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn aqr() -> PathBuf {
    let aqr = Path::new(env!("CARGO_BIN_EXE_aqr-mount-fuse")).with_file_name("aqr");
    assert!(aqr.is_file(), "{}: not built (cargo rt -p aqr -p aqr-mount-fuse)", aqr.display());
    aqr
}

fn fuse() -> bool {
    let tool = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join("fusermount3").is_file() || d.join("fusermount").is_file());
    Path::new("/dev/fuse").exists() && tool
}

fn mounted(at: &Path) -> bool {
    let at = at.to_string_lossy();
    fs::read_to_string("/proc/self/mounts").unwrap().lines().any(|l| l.split(' ').nth(1) == Some(&*at))
}

/// The program, killed as it is dropped (a test failed), then unmounted.
struct Mount(Child, PathBuf);

impl Drop for Mount {
    fn drop(&mut self) {
        _ = self.0.kill();
        _ = self.0.wait();
        if mounted(&self.1) {
            _ = Command::new("fusermount3").arg("-u").arg(&self.1).status();
        }
    }
}

#[test]
fn mount() {
    if !fuse() {
        eprintln!("skipped: no /dev/fuse or fusermount3");
        return;
    }
    let d = std::env::temp_dir().join(format!("aqr-mount-fuse-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("a.txt", b"alpha\n".repeat(1000)),
        ("sub/big.bin", (0..3u32 << 20).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()),
        ("sub/deep/c.txt", b"gamma".to_vec()),
        ("zero", Vec::new()),
    ];
    for (p, b) in &files {
        let p = d.join("t").join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b).unwrap();
    }
    let o = Command::new(aqr()).current_dir(&d).args(["-q", "-o", "t.aqr", "t"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let at = d.join("m");
    fs::create_dir(&at).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_aqr-mount-fuse"))
        .env("AQR", aqr())
        .arg(d.join("t.aqr"))
        .arg(&at)
        .spawn()
        .unwrap();
    let mut m = Mount(child, at.clone());
    let since = Instant::now();
    while !mounted(&at) {
        assert!(m.0.try_wait().unwrap().is_none(), "the program ended before mounting");
        assert!(since.elapsed() < Duration::from_secs(10), "not mounted");
        std::thread::sleep(Duration::from_millis(20));
    }

    for (p, b) in &files {
        assert!(fs::read(at.join(p)).unwrap() == *b, "{p}");
    }
    let mut names: Vec<_> = fs::read_dir(&at).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["a.txt", "sub", "zero"]);
    assert_eq!(fs::read(at.join("A.TXT")).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    let refused = fs::write(at.join("new"), b"x").unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(libc::EROFS));

    // SIGTERM: unmounted, and the program ends well
    assert_eq!(unsafe { libc::kill(m.0.id() as libc::pid_t, libc::SIGTERM) }, 0);
    let status = m.0.wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(!mounted(&at));
    drop(m);
    fs::remove_dir_all(&d).unwrap();
}
