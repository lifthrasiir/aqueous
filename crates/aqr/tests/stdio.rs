//! Standard input and output (`-`): compressing standard input (as an archive of a single file,
//! to standard output by default) and into standard output (an archive of several files
//! written as it is made); decompressing standard input, and into standard output the file of
//! an archive of a single file or one path given; what they are not taken with; no
//! temporaries left.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-stdio-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// aqr run in `d` on `args`, `input` its standard input (the temporary directory under `tmp`):
/// whether it succeeded, its standard output and error.
fn aqr(d: &Path, tmp: &Path, args: &[&str], input: &[u8]) -> (bool, Vec<u8>, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(d)
        .env("TMPDIR", tmp)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut i = c.stdin.take().unwrap();
    let input = input.to_vec();
    let feed = std::thread::spawn(move || _ = i.write_all(&input));
    let o = c.wait_with_output().unwrap();
    feed.join().unwrap();
    (o.status.success(), o.stdout, String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn through_stdio() {
    let d = scratch("through");
    let tmp = d.join("tmp");
    fs::create_dir(&tmp).unwrap();
    let text: Vec<u8> = (0..20000).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
    fs::create_dir_all(d.join("t/sub")).unwrap();
    fs::write(d.join("t/a.txt"), &text).unwrap();
    fs::write(d.join("t/sub/b.txt"), &text[..5000]).unwrap();

    // standard input, to standard output by default, and back
    let (ok, archive, err) = aqr(&d, &tmp, &["-q", "-"], &text);
    assert!(ok, "{err}");
    assert!(archive.len() < text.len() / 2);
    let (ok, out, err) = aqr(&d, &tmp, &["-dq", "-"], &archive);
    assert!(ok && out == text, "{err}");
    // (no input given: standard input, as it is not a terminal)
    let (ok, out, err) = aqr(&d, &tmp, &["-dq"], &archive);
    assert!(ok && out == text, "{err}");
    // into a file named
    let (ok, _, err) = aqr(&d, &tmp, &["-dq", "-", "-o", "x.txt"], &archive);
    assert!(ok && fs::read(d.join("x.txt")).unwrap() == text, "{err}");

    // a tree into standard output; its paths out of standard input
    let (ok, archive, err) = aqr(&d, &tmp, &["-q", "-o", "-", "t"], b"");
    assert!(ok, "{err}");
    fs::write(d.join("t.aqr"), &archive).unwrap();
    let (ok, _, err) = aqr(&d, &tmp, &["-Kq", "t.aqr"], b"");
    assert!(ok, "{err}");
    let (ok, out, err) = aqr(&d, &tmp, &["-dq", "-", "sub/b.txt"], &archive);
    assert!(ok && out == text[..5000], "{err}");
    let (ok, out, err) = aqr(&d, &tmp, &["-dq", "-o", "-", "t.aqr", "a.txt"], b"");
    assert!(ok && out == text, "{err}");
    let (ok, _, err) = aqr(&d, &tmp, &["-dq", "-", "-o", "out"], &archive);
    assert!(ok, "{err}");
    assert_eq!(fs::read(d.join("out/sub/b.txt")).unwrap(), &text[..5000]);
    // (with mirror blocks too)
    let (ok, archive, err) = aqr(&d, &tmp, &["-qA", "-o", "-", "t"], b"");
    assert!(ok, "{err}");
    let (ok, out, err) = aqr(&d, &tmp, &["-dq", "-", "a.txt"], &archive);
    assert!(ok && out == text, "{err}");

    for (args, told) in [
        (&["-q", "-", "t"][..], "the only input"),
        (&["-dq", "-o", "-", "t.aqr"], "-o - (standard output) of a PATH"),
        (&["-dq", "-o", "-", "t.aqr", "sub", "a.txt"], "2 files given"),
        (&["-q", "--rm", "-"], "--rm with -"),
        (&["-q", "-a", "-o", "-", "t"], "-a with -o -"),
    ] {
        let (ok, _, err) = aqr(&d, &tmp, args, b"");
        assert!(!ok && err.contains(told), "aqr {args:?}: {err}");
    }
    assert!(fs::read_dir(&tmp).unwrap().next().is_none(), "left in TMPDIR");
    fs::remove_dir_all(&d).unwrap();
}

/// `--passphrase-from`: of an environment variable, a file, standard input and a file
/// descriptor (Unix); what does not open an archive fails it, nothing asked for.
#[test]
fn passphrase_from() {
    let d = scratch("pass");
    let tmp = d.join("tmp");
    fs::create_dir(&tmp).unwrap();
    let text: Vec<u8> = (0..2000).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
    fs::write(d.join("a.txt"), &text).unwrap();
    fs::write(d.join("pass"), "sesame\r\nnot this line\n").unwrap();
    fs::write(d.join("wrong"), "open\n").unwrap();
    let run = |args: &[&str], input: &[u8]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"));
        c.current_dir(&d).env("TMPDIR", &tmp).env("AQR_PASS", "sesame").args(args);
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut c = c.spawn().unwrap();
        let mut i = c.stdin.take().unwrap();
        let input = input.to_vec();
        let feed = std::thread::spawn(move || _ = i.write_all(&input));
        let o = c.wait_with_output().unwrap();
        feed.join().unwrap();
        (o.status.success(), o.stdout, String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let (ok, _, err) = run(&["-q", "--passphrase-from=env:AQR_PASS", "-o", "a.aqr", "a.txt"], b"");
    assert!(ok, "{err}");
    for (args, input) in [
        (&["-dq", "--passphrase-from=pass", "-o", "-", "a.aqr"][..], &b""[..]),
        (&["-dq", "--passphrase-from", "stdin", "-o", "-", "a.aqr"], b"sesame\n"),
        (&["-dq", "--passphrase-from=env:AQR_PASS", "-"], &fs::read(d.join("a.aqr")).unwrap()),
    ] {
        let (ok, out, err) = run(args, input);
        assert!(ok && out == text, "aqr {args:?}: {err}");
    }
    #[cfg(unix)]
    {
        // (the descriptor of a file opened by the shell)
        let o = Command::new("sh")
            .current_dir(&d)
            .env("TMPDIR", &tmp)
            .arg("-c")
            .arg(format!("'{}' -dq --passphrase-from=fd:3 -o - a.aqr 3<pass", env!("CARGO_BIN_EXE_aqr")))
            .output()
            .unwrap();
        assert!(o.status.success() && o.stdout == text, "fd:3: {}", String::from_utf8_lossy(&o.stderr));
    }
    for (args, told) in [
        (&["-dq", "--passphrase-from=wrong", "-o", "-", "a.aqr"][..], "does not open"),
        (&["-dq", "--passphrase-from=env:AQR_NONE", "a.aqr"], "not set"),
        (&["-dq", "--passphrase-from=stdin", "-"], "the passphrase is read from it"),
        (&["-dq", "--passphrase-from=fd:x", "a.aqr"], "fd:N takes"),
    ] {
        let (ok, _, err) = run(args, b"");
        assert!(!ok && err.contains(told), "aqr {args:?}: {err}");
    }
    fs::remove_dir_all(&d).unwrap();
}
