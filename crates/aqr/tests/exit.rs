//! The exit codes of runs that fail, by what failed (`crates/aqr/src/exit.rs`): 1 otherwise,
//! 2 of the arguments, 3 an archive invalid, 4 not decrypted, 5 needing more than allowed, 7
//! done but for what is left out.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-exit-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn code(d: &Path, args: &[&str]) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).arg("-q").args(args).output().unwrap().status.code()
}

#[test]
fn codes() {
    let d = scratch("codes");
    fs::create_dir(d.join("t")).unwrap();
    fs::write(d.join("t/a.txt"), (0..50000).map(|n| format!("{n}\n")).collect::<String>()).unwrap();
    // (a gzip stream, recompressed by a module)
    let gz = Command::new("gzip").arg("-k").arg(d.join("t/a.txt")).status();
    assert_eq!(code(&d, &["-o", "t.aqr", "t"]), Some(0));
    assert_eq!(code(&d, &["--frobnicate", "t"]), Some(2));
    assert_eq!(code(&d, &["-d", "missing.aqr"]), Some(1));
    // a byte of its data changed
    let mut b = fs::read(d.join("t.aqr")).unwrap();
    let at = b.len() / 3;
    b[at] ^= 0xff;
    fs::write(d.join("bad.aqr"), &b).unwrap();
    assert_eq!(code(&d, &["-K", "bad.aqr"]), Some(3));
    assert_eq!(code(&d, &["-d", "-o", "bad", "bad.aqr"]), Some(3));
    // encrypted, the passphrase given not opening it
    fs::write(d.join("pass"), "sesame\n").unwrap();
    fs::write(d.join("wrong"), "open\n").unwrap();
    assert_eq!(code(&d, &["--passphrase-from=pass", "-o", "p.aqr", "t"]), Some(0));
    assert_eq!(code(&d, &["-d", "--passphrase-from=wrong", "-o", "p", "p.aqr"]), Some(4));
    if gz.is_ok_and(|s| s.success()) {
        assert_eq!(code(&d, &["-f", "-o", "t.aqr", "t"]), Some(0));
        assert_eq!(code(&d, &["-d", "--application-bound", "1K", "-o", "small", "t.aqr"]), Some(5));
    }
    // a single file decompressed over its archive: done, the archive not removed
    fs::write(d.join("f"), b"f").unwrap();
    assert_eq!(code(&d, &["f"]), Some(0));
    assert_eq!(code(&d, &["-d", "-f", "--rm", "-o", "f.aqr", "f.aqr"]), Some(7));
    fs::remove_dir_all(&d).unwrap();
}
