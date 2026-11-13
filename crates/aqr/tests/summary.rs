//! The summary `-c` prints ("N files, X -> Y (ratio)") counts the bytes of the files as `-d` and
//! `-K` do: each hard link of a file for its size, though the archive reads its content once.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    common::scratch("summary", name)
}

/// Standard error of `aqr` with `args` run in `dir`, which it has to succeed in.
fn aqr(dir: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "{err}");
    err
}

#[test]
fn hard_links_and_copies_counted_in_full() {
    let d = scratch("links");
    fs::create_dir(d.join("tree")).unwrap();
    let data: Vec<u8> = (0..100_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    fs::write(d.join("tree/a.bin"), &data).unwrap();
    // a hard link (one content for two files) and a copy (two contents of one hash)
    fs::hard_link(d.join("tree/a.bin"), d.join("tree/b.bin")).unwrap();
    fs::write(d.join("tree/c.bin"), &data).unwrap();

    let err = aqr(&d, &["-c", "-o", "out.aqr", "tree"]);
    assert!(err.contains("3 files, 300.0 KB -> "), "{err}");
    let err = aqr(&d, &["-K", "out.aqr"]);
    assert!(err.contains("3 files, 300.0 KB in "), "{err}");
    fs::remove_dir_all(&d).unwrap();
}
