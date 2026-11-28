//! An archive mounted through WinFsp on a directory (a junction to the volume, which needs no
//! administrator), as the program is run (the `aqr` built beside the tests: `cargo rt -p aqr -p
//! aqr-mount-win`): its files read whole and in another case, writes refused, the mount gone
//! with the process killed. Skipped where WinFsp is not installed (CI's runners).
#![cfg(windows)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn aqr() -> PathBuf {
    let aqr = Path::new(env!("CARGO_BIN_EXE_aqr-mount")).with_file_name("aqr.exe");
    assert!(aqr.is_file(), "{}: not built (cargo rt -p aqr -p aqr-mount-win)", aqr.display());
    aqr
}

/// The program, killed as it is dropped (a test failed).
struct Mount(Child);

impl Drop for Mount {
    fn drop(&mut self) {
        _ = self.0.kill();
        _ = self.0.wait();
    }
}

#[test]
fn mount() {
    let d = std::env::temp_dir().join(format!("aqr-mount-win-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("a.txt", b"alpha\r\n".repeat(1000)),
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
    // (WinFsp makes the directory: it must not be there)
    let at = d.join("m");

    let child = Command::new(env!("CARGO_BIN_EXE_aqr-mount"))
        .env("AQR", aqr())
        .arg(d.join("t.aqr"))
        .arg(&at)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut m = Mount(child);
    let since = Instant::now();
    while fs::read_dir(&at).map_or(true, |mut e| e.next().is_none()) {
        if let Some(status) = m.0.try_wait().unwrap() {
            let mut err = String::new();
            m.0.stderr.take().unwrap().read_to_string(&mut err).unwrap();
            if status.code() == Some(2) && err.contains("WinFsp is not installed") {
                eprintln!("skipped: {}", err.trim());
                fs::remove_dir_all(&d).unwrap();
                return;
            }
            panic!("the program ended before mounting ({status}): {err}");
        }
        assert!(since.elapsed() < Duration::from_secs(10), "not mounted");
        std::thread::sleep(Duration::from_millis(20));
    }

    for (p, b) in &files {
        assert!(fs::read(at.join(p)).unwrap() == *b, "{p}");
    }
    let mut names: Vec<_> = fs::read_dir(&at).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["a.txt", "sub", "zero"]);
    assert!(fs::read(at.join("SUB\\DEEP\\C.TXT")).unwrap() == b"gamma");
    // (ERROR_WRITE_PROTECT)
    assert_eq!(fs::write(at.join("new"), b"x").unwrap_err().raw_os_error(), Some(19));
    assert!(fs::OpenOptions::new().append(true).open(at.join("a.txt")).is_err());

    // killed: the volume gone, and the directory WinFsp made with it
    drop(m);
    let since = Instant::now();
    while at.exists() {
        assert!(since.elapsed() < Duration::from_secs(10), "{}: still there", at.display());
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::remove_dir_all(&d).unwrap();
}
