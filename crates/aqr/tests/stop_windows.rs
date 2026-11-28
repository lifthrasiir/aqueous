//! write/3 on Windows: an archive stopped as it is written by Ctrl-Break (`aqr` run in a process
//! group of its own, sent `CTRL_BREAK_EVENT` by `GenerateConsoleCtrlEvent`: what `src/stop.rs`
//! handles); the counterparts of `append.rs`'s `stopped` and `append_stopped_and_resumed`, and
//! `write.rs`'s `atomic_append_stopped`.
//!
//! The event goes to the console `aqr` is attached to, so `aqr` has a console of its own (hidden),
//! and a helper process (this test binary run again, `BREAK_HELPER`) attaches to it and sends the
//! event: what this process is attached to (maybe no console at all) is left alone.
#![cfg(windows)]

mod common;
use common::{files, stderr, text};

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Console::{
    AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
};

const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x8;

/// Set (to a process id) for the helper that sends Ctrl-Break to that process's group (PID:COUNT).
const BREAK_HELPER: &str = "AQR_TEST_BREAK_HELPER";

/// Not a test of its own: the helper process the tests below run this binary as.
#[test]
fn break_helper() {
    let Ok(arg) = std::env::var(BREAK_HELPER) else { return };
    let (pid, count) = arg.split_once(':').unwrap();
    let (pid, count): (u32, u32) = (pid.parse().unwrap(), count.parse().unwrap());
    // SAFETY: console calls of this process alone (detached, so attached to no console before)
    unsafe {
        FreeConsole();
        if AttachConsole(pid) == 0 {
            std::process::exit(2);
        }
        // (not to take the event itself)
        SetConsoleCtrlHandler(None, 1);
        for i in 0..count {
            if GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) == 0 {
                // (a later one finding the process ended by the first: nothing to stop)
                std::process::exit(if i == 0 { 3 } else { 0 });
            }
        }
    }
}

fn send_break(pid: u32, count: usize) {
    let s = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "break_helper", "--nocapture", "--test-threads=1"])
        .env(BREAK_HELPER, format!("{pid}:{count}"))
        .creation_flags(DETACHED_PROCESS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(s.success(), "Ctrl-Break not sent: {s}");
}

fn scratch(name: &str) -> PathBuf {
    common::scratch("stopwin", name)
}

fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

/// `aqr` given `args` in `dir`, sent Ctrl-Break `breaks` times once `when` holds (or 5 seconds have
/// gone by): its outcome, and whether it was still running when they were sent.
fn broken(dir: &Path, args: &[&str], when: &dyn Fn() -> bool, breaks: usize) -> (Output, bool) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(dir)
        .args(args)
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !when() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
    }
    let running = c.try_wait().unwrap().is_none();
    if running {
        send_break(c.id(), breaks);
    }
    (c.wait_with_output().unwrap(), running)
}

/// Many small files, archived a segment a batch, slowly enough to be stopped.
fn many(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..300 {
        fs::write(src.join(format!("f{i:03}.txt")), text(i, 40_000)).unwrap();
    }
    files(&src)
}

const SLOW: [&str; 8] = ["-T", "1", "-B", "100K", "-S", "1", "-o", "b.aqr"];

/// An archive stopped by Ctrl-Break once a segment of it is committed: ended as it is, of the
/// files written, telling what it left out. Stopped twice, at once: the archive of its last
/// commit. With `--atomic`, nothing.
#[test]
fn stopped() {
    let d = scratch("stopped");
    let all = many(&d);
    let out = d.join("b.aqr");
    for breaks in [1, 2] {
        let _ = fs::remove_file(&out);
        let args: Vec<&str> = SLOW.iter().copied().chain(["src"]).collect();
        let (o, running) = broken(&d, &args, &|| out.exists(), breaks);
        assert!(running, "done before it was stopped");
        if breaks == 1 {
            assert_eq!(o.status.code(), Some(130), "{}", stderr(&o));
            assert!(stderr(&o).contains("aqr: stopped: b.aqr holds"), "{:?} {}", o.status, stderr(&o));
        }
        let t = ok(&d, &["-K", "b.aqr"]);
        assert!(stderr(&t).contains("incomplete, its writing stopped"), "{}", stderr(&t));
        ok(&d, &["-d", "-f", "-o", "x", "b.aqr"]);
        let got = files(&d.join("x"));
        fs::remove_dir_all(d.join("x")).unwrap();
        assert!(!got.is_empty() && got.len() < all.len(), "{}", got.len());
        assert!(got.iter().all(|f| all.contains(f)));
        let parts =
            fs::read_dir(&d).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".part"));
        assert_eq!(parts.count(), 0);
    }
    // with --atomic, nothing
    let _ = fs::remove_file(&out);
    let args: Vec<&str> = ["--atomic"].into_iter().chain(SLOW).chain(["src"]).collect();
    let start = Instant::now();
    let (o, running) = broken(&d, &args, &|| start.elapsed() > Duration::from_millis(500), 1);
    assert!(running && !o.status.success());
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("b.aqr")));
    fs::remove_dir_all(&d).unwrap();
}

/// The first `n` files of `src` (of `many`) in `half`, with their times.
fn copy_of(d: &Path, n: usize) {
    let (src, half) = (d.join("src"), d.join("half"));
    fs::create_dir_all(&half).unwrap();
    let mut names: Vec<_> = fs::read_dir(&src).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    for name in names.into_iter().take(n) {
        fs::copy(src.join(&name), half.join(&name)).unwrap();
        let m = fs::metadata(src.join(&name)).unwrap().modified().unwrap();
        fs::OpenOptions::new().write(true).open(half.join(&name)).unwrap().set_modified(m).unwrap();
    }
}

fn extracted(d: &Path, archive: &str) -> Vec<(String, Vec<u8>)> {
    let _ = fs::remove_dir_all(d.join("x"));
    ok(d, &["-d", "-f", "-o", "x", archive]);
    let got = files(&d.join("x"));
    fs::remove_dir_all(d.join("x")).unwrap();
    got
}

/// An append stopped by Ctrl-Break ends what it wrote as a commit; appending again adds the rest.
#[test]
fn append_stopped_and_resumed() {
    let d = scratch("resumed");
    many(&d);
    copy_of(&d, 100);
    ok(&d, &["-q", "-o", "b.aqr", "half"]);
    let out = d.join("b.aqr");
    let len = fs::metadata(&out).unwrap().len();
    let args: Vec<&str> = ["-a"].into_iter().chain(SLOW).chain(["src"]).collect();
    let (o, running) = broken(&d, &args, &|| fs::metadata(&out).unwrap().len() > len + 200_000, 1);
    assert!(running, "done before it was stopped");
    assert_eq!(o.status.code(), Some(130), "{}", stderr(&o));
    assert!(stderr(&o).contains(") appended to b.aqr"), "{:?} {}", o.status, stderr(&o));
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(stderr(&t).contains("incomplete"), "{}", stderr(&t));
    let got = extracted(&d, "b.aqr");
    assert!(got.len() > 100 && got.len() < 300, "{}", got.len());
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
    fs::remove_dir_all(&d).unwrap();
}

/// An append with `--atomic` stopped by Ctrl-Break (once, and twice) leaves the archive byte for
/// byte as it was, and no part of it beside.
#[test]
fn atomic_append_stopped() {
    for breaks in [1, 2] {
        let d = scratch(&format!("atomic-append-{breaks}"));
        many(&d);
        copy_of(&d, 50);
        ok(&d, &["-q", "-o", "b.aqr", "half"]);
        let before = fs::read(d.join("b.aqr")).unwrap();
        let start = Instant::now();
        let (o, running) = broken(
            &d,
            &["--atomic", "-a", "-T", "1", "-B", "100K", "-S", "1", "b.aqr", "src"],
            &|| start.elapsed() > Duration::from_millis(500),
            breaks,
        );
        assert!(running, "done before it was stopped");
        assert!(!o.status.success(), "{}", stderr(&o));
        assert_eq!(fs::read(d.join("b.aqr")).unwrap(), before, "{breaks}: {}", stderr(&o));
        assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
        // (and appended to whole after)
        ok(&d, &["-q", "-a", "b.aqr", "src"]);
        assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
        fs::remove_dir_all(&d).unwrap();
    }
}

/// An append cancelled by a frontend (`--ipc`'s cancel) after it committed some segments: the
/// archive is cut back to what it was, byte for byte (the commits undone, the footer it made a
/// spacer of restored), where it can still be appended to.
#[test]
fn ipc_append_cancelled_restores() {
    use common::frontend;
    let d = scratch("ipc-cancel");
    many(&d);
    copy_of(&d, 50);
    ok(&d, &["-q", "-o", "b.aqr", "half"]);
    let out = d.join("b.aqr");
    let before = fs::read(&out).unwrap();
    let args = ["-a", "-T", "1", "-B", "100K", "-S", "1", "b.aqr", "src"].map(std::ffi::OsStr::new);
    let (mut c, rx) = frontend::start(Some(&d), &args, &[], Stdio::null());
    let mut lines = rx.into_iter();
    assert_eq!(lines.next().unwrap()["event"], "begin");
    // (some segments committed)
    let start = Instant::now();
    while fs::metadata(&out).unwrap().len() < before.len() as u64 + 200_000 && start.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(c.try_wait().unwrap().is_none(), "done before it was cancelled");
    frontend::send(&mut c, &serde_json::json!({"control": "cancel"}));
    let rest: Vec<serde_json::Value> = lines.filter(|v| v["event"] != "progress").collect();
    assert_eq!(c.wait().unwrap().code(), Some(130), "{rest:?}");
    assert!(rest.iter().any(|v| v["event"] == "stopped"), "{rest:?}");
    assert_eq!(fs::read(&out).unwrap(), before);
    let t = ok(&d, &["-K", "b.aqr"]);
    assert!(!stderr(&t).contains("footer made again"), "{}", stderr(&t));
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    assert_eq!(extracted(&d, "b.aqr"), files(&d.join("src")));
    fs::remove_dir_all(&d).unwrap();
}
