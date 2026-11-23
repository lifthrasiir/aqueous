//! Archives converted (`--convert`): Aqueous archives written again with their encryption and
//! mirror blocks put on, changed or taken off, repaired where damaged, kept as they are
//! otherwise (segments, journal points).

mod common;
use common::{files, stderr, text};

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    common::scratch("convert", name)
}

/// `aqr` given `args` in `dir`, the environment's `PA` and `PB` set (for `--passphrase-from`).
fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(dir)
        .args(args)
        .env("PA", "first")
        .env("PB", "second")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    stderr(&o)
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let o = aqr(dir, args);
    assert!(!o.status.success(), "aqr {args:?} did not fail: {}", stderr(&o));
    stderr(&o)
}

/// An archive `a.aqr` in `d` of three segments (two appends, the first naming a journal point),
/// and the files it holds.
fn archive(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    for i in 0..4u64 {
        fs::write(src.join(format!("t{i}.txt")), text(10 + i, 4000 + 900 * i as usize)).unwrap();
    }
    ok(d, &["-q", "-o", "a.aqr", "src"]);
    fs::write(src.join("sub/u.txt"), text(99, 20_000)).unwrap();
    ok(d, &["-q", "-a", "-j", "one", "a.aqr", "src"]);
    fs::write(src.join("t0.txt"), text(7, 5000)).unwrap();
    ok(d, &["-q", "-a", "a.aqr", "src"]);
    files(&src)
}

/// The files of archive `name` in `d`, extracted (`pass`: as `--passphrase-from` gives it).
fn extracted(d: &Path, name: &str, pass: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let to = d.join("x");
    let _ = fs::remove_dir_all(&to);
    let mut args = vec!["-q", "-d", "-o", "x", name];
    if let Some(p) = pass {
        args.extend(["--passphrase-from", p]);
    }
    ok(d, &args);
    let got = files(&to);
    fs::remove_dir_all(&to).unwrap();
    got
}

/// What `-I` and `-K` tell of the archive's mirror blocks: whether it is mirrored.
fn mirrored(d: &Path, name: &str, pass: Option<&str>) -> bool {
    let mut args = vec!["-K", name];
    if let Some(p) = pass {
        args.extend(["--passphrase-from", p]);
    }
    ok(d, &args).contains("mirrored archive: yes")
}

fn history(d: &Path, name: &str, pass: Option<&str>) -> String {
    let mut args = vec!["--history", name];
    if let Some(p) = pass {
        args.extend(["--passphrase-from", p]);
    }
    let o = aqr(d, &args);
    assert!(o.status.success(), "{}", stderr(&o));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn encrypted(d: &Path, name: &str) -> bool {
    fs::read(d.join(name)).unwrap().starts_with(&[0xf9, 0xca, 0xf2, 0xa6, 0xd0, 0x2a])
}

/// Mirror blocks put on and taken off, an encryption put on, its passphrase changed and taken
/// off, all in place but the last (`-o`): the files and the journal point kept each time; with
/// nothing to change, nothing written.
#[test]
fn settings_changed() {
    let d = scratch("settings");
    let want = archive(&d);
    let points = history(&d, "a.aqr", None);
    assert!(points.contains("one"), "{points}");
    let before = fs::read(d.join("a.aqr")).unwrap();
    let t = ok(&d, &["--convert", "a.aqr"]);
    assert!(t.contains("nothing to change"), "{t}");
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), before);
    assert!(!mirrored(&d, "a.aqr", None));

    let t = ok(&d, &["--convert", "-A", "a.aqr"]);
    assert!(t.contains("mirrored"), "{t}");
    assert!(mirrored(&d, "a.aqr", None));
    assert_eq!(extracted(&d, "a.aqr", None), want);
    assert_eq!(history(&d, "a.aqr", None), points);
    // (mirrored already)
    assert!(ok(&d, &["--convert", "-A", "a.aqr"]).contains("nothing to change"));

    // encrypted, its mirror blocks inherited
    let t = ok(&d, &["--convert", "-P", "--passphrase-from", "env:PA", "a.aqr"]);
    assert!(t.contains("encrypted"), "{t}");
    assert!(encrypted(&d, "a.aqr"));
    assert!(mirrored(&d, "a.aqr", Some("env:PA")));
    assert_eq!(extracted(&d, "a.aqr", Some("env:PA")), want);
    assert_eq!(history(&d, "a.aqr", Some("env:PA")), points);
    // its mirror blocks taken off, the passphrase inherited
    ok(&d, &["--convert", "--no-archival", "--passphrase-from", "env:PA", "a.aqr"]);
    assert!(encrypted(&d, "a.aqr") && !mirrored(&d, "a.aqr", Some("env:PA")));
    assert!(ok(&d, &["--convert", "--passphrase-from", "env:PA", "a.aqr"]).contains("nothing to change"));
    // the passphrase changed: the input's, then -P's
    let t = ok(&d, &["--convert", "-P", "--passphrase-from", "env:PA;env:PB", "a.aqr"]);
    assert!(t.contains("its passphrase changed"), "{t}");
    fails(&d, &["-K", "--passphrase-from", "env:PA", "a.aqr"]);
    assert_eq!(extracted(&d, "a.aqr", Some("env:PB")), want);
    // decrypted, to another file
    let sealed = fs::read(d.join("a.aqr")).unwrap();
    ok(&d, &["--convert", "--no-passphrase", "--passphrase-from", "env:PB", "-o", "b.aqr", "a.aqr"]);
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), sealed);
    assert!(!encrypted(&d, "b.aqr"));
    assert_eq!(extracted(&d, "b.aqr", None), want);
    assert_eq!(history(&d, "b.aqr", None), points);
    // (and what it held made of three segments still: appended to)
    ok(&d, &["-q", "-a", "b.aqr", "src"]);
    ok(&d, &["-q", "--compact", "--drop-history", "b.aqr"]);
    assert_eq!(extracted(&d, "b.aqr", None), want);

    // the journal dropped, as --compact drops it
    let t = ok(&d, &["--convert", "--drop-history", "--passphrase-from", "env:PB", "a.aqr"]);
    assert!(t.contains("journal points dropped"), "{t}");
    let t = fails(&d, &["--history", "--passphrase-from", "env:PB", "a.aqr"]);
    assert!(t.contains("no journal points"), "{t}");
    assert_eq!(extracted(&d, "a.aqr", Some("env:PB")), want);
    fs::remove_dir_all(&d).unwrap();
}

/// The passphrases' sources counted against those needed; options refused, the archive left as
/// it is.
#[test]
fn refused() {
    let d = scratch("refused");
    archive(&d);
    let before = fs::read(d.join("a.aqr")).unwrap();
    for (args, why) in [
        (&["--convert", "-P", "--passphrase-from", "env:PA;env:PB", "a.aqr"][..], "2 sources given, 1 passphrase"),
        (&["--convert", "--passphrase-from", "env:PA", "-A", "a.aqr"], "1 source given, 0 passphrases"),
        (&["--convert", "-9", "a.aqr"], "-l: an Aqueous archive converted is not compressed again"),
        (&["--convert", "-S", "1M", "a.aqr"], "-S: an Aqueous archive converted is not"),
        (&["--convert", "-P", "--no-passphrase", "a.aqr"], "-P with --no-passphrase"),
        (&["--convert", "-A", "--no-archival", "a.aqr"], "-A with --no-archival"),
        (&["--convert", "--compact", "a.aqr"], "--convert with --compact"),
        (&["--convert", "-o", "x.aqr", "a.aqr", "a.aqr"], "-o with several inputs"),
        (
            &["-c", "--passphrase-from", "env:PA;env:PB", "-o", "y.aqr", "src"],
            "two sources (apart by ;) are of --convert",
        ),
        (&["-c", "-A", "--no-archival", "-o", "y.aqr", "src"], "-A with --no-archival"),
    ] {
        let t = fails(&d, args);
        assert!(t.contains(why), "{args:?}: {t}");
    }
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), before);
    // (elsewhere, asking for the default)
    ok(&d, &["-q", "--no-passphrase", "--no-archival", "-o", "z.aqr", "src"]);
    assert!(!encrypted(&d, "z.aqr") && !mirrored(&d, "z.aqr", None));
    // after a stub (a self-extracting program's)
    fs::write(d.join("lead.aqr"), [b"#!stub\n".as_slice(), &before].concat()).unwrap();
    let t = fails(&d, &["--convert", "-A", "lead.aqr"]);
    assert!(t.contains("after 7 bytes of other data"), "{t}");
    fs::remove_dir_all(&d).unwrap();
}

/// The blocks of archive `a`, from its header to its footer: where each starts, its type and
/// where its data is.
fn blocks(a: &[u8]) -> Vec<(usize, u64, Range<usize>)> {
    let leb = |at: &mut usize| {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = a[*at];
            *at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    };
    let mut out = Vec::new();
    let mut at = 6;
    loop {
        let start = at;
        let ty = leb(&mut at);
        if ty == 0x90_5bcb_957d {
            return out;
        }
        let width = 1 << (ty % 4);
        let size = u64::from_le_bytes(std::array::from_fn(|i| if i < width { a[at + i] } else { 0 }));
        at += width;
        let data = at..at + size as usize;
        at = data.end + (data.end - start) % 2;
        out.push((start, ty, data));
    }
}

/// A damaged archive of mirror blocks written again repaired with nothing else given (its file
/// repaired for good: read with nothing restored after); one whose damage its mirror blocks do
/// not restore (a leaf block's) not written, left as it is.
#[test]
fn repaired() {
    let d = scratch("repaired");
    let want = archive(&d);
    ok(&d, &["--convert", "-A", "a.aqr"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    let b = blocks(&a);
    let meta = b.iter().find(|b| (0x10..0x18).contains(&b.1)).unwrap().2.clone();
    let mut damaged = a.clone();
    damaged[(meta.start + meta.end) / 2] ^= 0x5a;
    fs::write(d.join("a.aqr"), &damaged).unwrap();
    let t = ok(&d, &["--convert", "a.aqr"]);
    assert!(t.contains("1 damaged block restored from mirror blocks") && t.contains("repaired"), "{t}");
    let t = ok(&d, &["-K", "a.aqr"]);
    assert!(!t.contains("restored") && t.contains("mirrored archive: yes"), "{t}");
    assert_eq!(extracted(&d, "a.aqr", None), want);

    // a leaf data block's damage (no mirror block of it)
    let a = fs::read(d.join("a.aqr")).unwrap();
    let leaf = blocks(&a).into_iter().find(|b| (0x20..0x38).contains(&b.1)).unwrap().2;
    let mut damaged = a.clone();
    damaged[(leaf.start + leaf.end) / 2] ^= 0x5a;
    // (and a metadata block's, for it to be found damaged as it is opened)
    damaged[(meta.start + meta.end) / 2] ^= 0x5a;
    fs::write(d.join("a.aqr"), &damaged).unwrap();
    let t = fails(&d, &["--convert", "a.aqr"]);
    assert!(t.contains("not converted"), "{t}");
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), damaged);
    fs::remove_dir_all(&d).unwrap();
}
