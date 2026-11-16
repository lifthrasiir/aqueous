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
    // a passphrase given, the archive not encrypted (one put in place of the encrypted archive)
    assert_eq!(code(&d, &["-d", "--passphrase-from=pass", "-o", "plain", "t.aqr"]), Some(4));
    assert_eq!(code(&d, &["-K", "--passphrase-from=pass", "t.aqr"]), Some(4));
    fs::remove_dir_all(d.join("plain")).ok();
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

/// An encrypted archive cut short at the end of one of its chunks (every chunk left
/// authenticating, the last as not the last): not taken as it is, but told and failing.
#[test]
fn encrypted_cut_at_a_chunk() {
    let d = scratch("cut");
    fs::create_dir(d.join("t")).unwrap();
    let mut x = 1u64;
    let noise: Vec<u8> = (0..300_000)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect();
    fs::write(d.join("t/n.bin"), &noise).unwrap();
    fs::write(d.join("pass"), "sesame\n").unwrap();
    assert_eq!(code(&d, &["--passphrase-from=pass", "-o", "p.aqr", "t"]), Some(0));
    let b = fs::read(d.join("p.aqr")).unwrap();
    // (after chunk 1: a header of 105 bytes, of 113 mirrored, then a mirrored header of 117
    // before chunk 1; chunks of 64 KiB and a tag)
    let cut = if b[6] == 2 { 113 + 117 } else { 105 } + 2 * (65_536 + 16);
    assert!(b.len() > cut + 65_552);
    fs::write(d.join("cut.aqr"), &b[..cut]).unwrap();
    for args in
        [&["-K", "--passphrase-from=pass", "cut.aqr"][..], &["-d", "--passphrase-from=pass", "-o", "c", "cut.aqr"]]
    {
        let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(&d).args(args).output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(3), "{args:?}: {err}");
        assert!(err.contains("cut short after an encrypted chunk"), "{args:?}: {err}");
    }
    fs::remove_dir_all(&d).unwrap();
}
