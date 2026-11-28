//! An archive mounted through WinFsp on a directory (a junction to the volume, which needs no
//! administrator), as the program is run (the `aqr` built beside the tests: `cargo rt -p aqr -p
//! aqr-mount-win`): its files read whole and in another case, writes refused, the mount gone
//! with the process killed; and ejected through its control, refused while a file is open but
//! where forced. Skipped where WinFsp is not installed (CI's runners).
#![cfg(windows)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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

fn files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.txt", b"alpha\r\n".repeat(1000)),
        ("sub/big.bin", (0..3u32 << 20).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()),
        ("sub/deep/c.txt", b"gamma".to_vec()),
        ("zero", Vec::new()),
    ]
}

/// The archive of [`files`] made in `d`, mounted at `d\m`: the program, the mount point and the
/// name of its control; none where WinFsp is not installed.
fn mounted(d: &Path) -> Option<(Mount, PathBuf, String)> {
    _ = fs::remove_dir_all(d);
    for (p, b) in files() {
        let p = d.join("t").join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b).unwrap();
    }
    let o = Command::new(aqr()).current_dir(d).args(["-q", "-o", "t.aqr", "t"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // (WinFsp makes the directory: it must not be there)
    let at = d.join("m");
    let child = Command::new(env!("CARGO_BIN_EXE_aqr-mount"))
        .env("AQR", aqr())
        .arg("--json")
        .arg(d.join("t.aqr"))
        .arg(&at)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut m = Mount(child);
    let mut line = String::new();
    BufReader::new(m.0.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    let told: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("told: {line}"));
    if let Some(e) = told["error"].as_str() {
        assert_eq!(m.0.wait().unwrap().code(), Some(2), "{e}");
        assert!(e.contains("WinFsp is not installed"), "{e}");
        eprintln!("skipped: {e}");
        fs::remove_dir_all(d).unwrap();
        return None;
    }
    assert_eq!(told["mounted"], at.to_str().unwrap());
    let control = told["control"].as_str().unwrap().to_owned();
    Some((m, at, control))
}

/// Request `q` of the control `pipe`: its answer.
fn ask(pipe: &str, q: Value) -> Value {
    let mut p = fs::OpenOptions::new().read(true).write(true).open(pipe).unwrap();
    writeln!(p, "{q}").unwrap();
    let mut line = String::new();
    BufReader::new(p).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn gone(at: &Path) {
    let since = Instant::now();
    while at.exists() {
        assert!(since.elapsed() < Duration::from_secs(10), "{}: still there", at.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn mount() {
    let d = std::env::temp_dir().join(format!("aqr-mount-win-{}", std::process::id()));
    let Some((m, at, _)) = mounted(&d) else { return };
    for (p, b) in files() {
        assert!(fs::read(at.join(p)).unwrap() == b, "{p}");
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
    gone(&at);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn eject() {
    let d = std::env::temp_dir().join(format!("aqr-mount-win-eject-{}", std::process::id()));
    let Some((mut m, at, control)) = mounted(&d) else { return };
    let archive = d.join("t.aqr");
    let status = ask(&control, json!({"status": true}));
    assert_eq!(status["archive"], archive.to_str().unwrap());
    assert_eq!(status["mount"], at.to_str().unwrap());
    assert_eq!(status["open"], 0);

    // (a directory listed counts for nothing; files open do, until they are closed)
    _ = fs::read_dir(&at).unwrap().count();
    let mut held = fs::File::open(at.join("a.txt")).unwrap();
    let other = fs::File::open(at.join("zero")).unwrap();
    assert_eq!(ask(&control, json!({"status": true}))["open"], 2);
    drop(other);
    assert_eq!(ask(&control, json!({"status": true}))["open"], 1);
    assert_eq!(ask(&control, json!({"eject": true})), json!({"open": 1}));
    // (refused: still there)
    let mut first = [0; 5];
    held.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"alpha");

    assert_eq!(ask(&control, json!({"eject": true, "force": true})), json!({"ejected": true}));
    let status = m.0.wait().unwrap();
    assert!(status.success(), "{status}");
    gone(&at);
    // (the file held may still read what the cache has of it: no more is asserted of it)
    drop(held);
    drop(m);
    fs::remove_dir_all(&d).unwrap();
}
