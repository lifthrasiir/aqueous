//! `aqr --ipc` driven as a frontend drives it: the hello, the events of an archive, the
//! overwrite question answered both ways (and not taken), and the work cancelled by a message
//! and by standard input closed.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A directory of its own for test `name`, empty.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-ipc-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// `aqr --ipc` with `args`, greeted with the hello taking `answers`: the process and its
/// messages, the answer to the hello read.
fn start(args: &[&std::ffi::OsStr], answers: &[&str]) -> (Child, impl Iterator<Item = Value> + use<>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .arg("--ipc")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    send(&mut c, json!({"hello": 1, "answers": answers}));
    let out: ChildStdout = c.stdout.take().unwrap();
    let mut lines = BufReader::new(out).lines().map(|l| serde_json::from_str::<Value>(&l.unwrap()).unwrap());
    let hello = lines.next().unwrap();
    assert_eq!(hello["hello"], 1);
    assert_eq!(hello["controls"], json!(["cancel"]));
    (c, lines)
}

fn send(c: &mut Child, v: Value) {
    let i = c.stdin.as_mut().unwrap();
    writeln!(i, "{v}").unwrap();
    i.flush().unwrap();
}

/// The events (not `progress`) and questions until the end, the questions answered `answer`.
fn run(c: &mut Child, lines: impl Iterator<Item = Value>, answer: bool) -> Vec<Value> {
    let mut seen = Vec::new();
    for v in lines {
        if v.get("ask").is_some() {
            send(c, json!({"answer": v["ask"], "value": answer}));
        }
        if v["event"] != "progress" {
            seen.push(v);
        }
    }
    seen
}

fn events(seen: &[Value]) -> Vec<&str> {
    seen.iter().map(|v| v["event"].as_str().or(v["kind"].as_str()).unwrap()).collect()
}

#[test]
fn overwrite() {
    let d = scratch("overwrite");
    let (input, out) = (d.join("in.txt"), d.join("in.txt.aqr"));
    fs::write(&input, "Aqueous ".repeat(1000)).unwrap();

    let (mut c, lines) = start(&[input.as_os_str()], &["overwrite"]);
    let seen = run(&mut c, lines, false);
    assert!(c.wait().unwrap().success());
    assert_eq!(events(&seen), ["begin", "end"]);
    assert_eq!(seen[0]["mode"], "compress");
    assert_eq!(seen[1]["read"], 8000);
    let made = fs::read(&out).unwrap();

    // declined: skipped, the archive as it was
    fs::write(&out, b"there before").unwrap();
    let (mut c, lines) = start(&[input.as_os_str()], &["overwrite"]);
    let seen = run(&mut c, lines, false);
    assert!(c.wait().unwrap().success());
    assert_eq!(events(&seen), ["overwrite", "skipped"]);
    assert_eq!(seen[0]["path"], out.display().to_string());
    assert_eq!(fs::read(&out).unwrap(), b"there before");

    // accepted: overwritten
    let (mut c, lines) = start(&[input.as_os_str()], &["overwrite"]);
    let seen = run(&mut c, lines, true);
    let st = c.wait().unwrap();
    assert!(st.success(), "{st} {seen:?}");
    assert_eq!(events(&seen), ["overwrite", "begin", "end"]);
    assert_eq!(fs::read(&out).unwrap(), made);

    // not taken by the frontend: an error, as without --ipc
    let (mut c, lines) = start(&[input.as_os_str()], &[]);
    let seen = run(&mut c, lines, true);
    assert!(!c.wait().unwrap().success());
    assert_eq!(events(&seen), ["error"]);
    assert!(seen[0]["message"].as_str().unwrap().contains("exists"));

    // decoded into a directory there already, its files overwritten as answered
    let (src, archive, x) = (d.join("src"), d.join("src.aqr"), d.join("x"));
    fs::create_dir(&src).unwrap();
    fs::write(src.join("f"), b"new").unwrap();
    let made = Command::new(env!("CARGO_BIN_EXE_aqr")).arg("-q").arg(&src).status().unwrap();
    assert!(made.success());
    fs::create_dir(&x).unwrap();
    fs::write(x.join("f"), b"old").unwrap();
    let (mut c, lines) = start(&["-d".as_ref(), "-o".as_ref(), x.as_os_str(), archive.as_os_str()], &["overwrite"]);
    let seen = run(&mut c, lines, true);
    assert!(c.wait().unwrap().success());
    assert_eq!(events(&seen), ["overwrite", "begin", "end"]);
    assert_eq!(seen[1]["mode"], "decompress");
    assert_eq!(fs::read(x.join("f")).unwrap(), b"new");
    fs::remove_dir_all(&d).unwrap();
}

/// `--auto-dir` (as the frontends extract): the output not told as it begins but once it is
/// decided (the single entry at the top, or the archive's directory), and asked of then.
#[test]
fn auto_dir() {
    let d = scratch("auto-dir");
    fs::create_dir_all(d.join("src/inner")).unwrap();
    fs::write(d.join("src/inner/f"), b"f").unwrap();
    let made = Command::new(env!("CARGO_BIN_EXE_aqr")).arg("-q").arg(d.join("src")).status().unwrap();
    assert!(made.success());
    fs::remove_dir_all(d.join("src")).unwrap();
    let archive = d.join("src.aqr");
    let args = ["-d".as_ref(), "--auto-dir".as_ref(), archive.as_os_str()];
    for out in ["inner", "src"] {
        let (mut c, lines) = start(&args, &["overwrite"]);
        let seen = run(&mut c, lines, false);
        assert!(c.wait().unwrap().success());
        assert_eq!(events(&seen), ["begin", "output", "end"], "{out}");
        assert_eq!(seen[0]["output"], Value::Null);
        assert_eq!(seen[1]["output"], d.join(out).display().to_string());
        assert_eq!(seen[2]["output"], d.join(out).display().to_string());
    }
    // both there: asked of the archive's directory, and skipped as answered
    let (mut c, lines) = start(&args, &["overwrite"]);
    let seen = run(&mut c, lines, false);
    assert!(c.wait().unwrap().success());
    assert_eq!(events(&seen), ["begin", "overwrite", "skipped"]);
    assert_eq!(seen[1]["path"], d.join("src").display().to_string());
    assert_eq!(fs::read(d.join("src/inner/f")).unwrap(), b"f");
    fs::remove_dir_all(&d).unwrap();
}

/// Cancelled once begun, by a message (`by_message`) or by standard input closed: stopped,
/// nothing left of the archive.
fn cancel(by_message: bool) {
    let d = scratch(if by_message { "cancel" } else { "eof" });
    let input = d.join("in");
    // (of some MB, not done before the cancel comes)
    let mut x = 1u64;
    let data: Vec<u8> = (0..32 << 20)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            b"abcdefgh \n"[(x >> 60) as usize % 10]
        })
        .collect();
    fs::write(&input, data).unwrap();
    let (mut c, mut lines) = start(&[input.as_os_str()], &[]);
    assert_eq!(lines.next().unwrap()["event"], "begin");
    if by_message {
        send(&mut c, json!({"control": "cancel"}));
    } else {
        drop(c.stdin.take());
    }
    let rest: Vec<Value> = lines.filter(|v| v["event"] != "progress").collect();
    assert_eq!(c.wait().unwrap().code(), Some(130));
    assert_eq!(events(&rest), ["stopped"]);
    let left: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, ["in"]);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn cancelled() {
    cancel(true);
}

#[test]
fn input_closed() {
    cancel(false);
}

/// The events (not `progress`) and questions until the end, each passphrase asked for answered
/// by the next of `answers` (`null` past them).
fn run_passphrases(c: &mut Child, lines: impl Iterator<Item = Value>, answers: &[Value]) -> Vec<Value> {
    let mut answers = answers.iter();
    let mut seen = Vec::new();
    for v in lines {
        if v["kind"] == "passphrase" {
            send(c, json!({"answer": v["ask"], "value": answers.next().cloned().unwrap_or(Value::Null)}));
        }
        if v["event"] != "progress" {
            seen.push(v);
        }
    }
    seen
}

/// `-P` asking the frontend as it begins (to confirm, compressing), and an encrypted archive
/// asking as it is opened (again where wrong, until the frontend gives up).
#[test]
fn passphrase() {
    let d = scratch("passphrase");
    let (input, archive, out) = (d.join("in.txt"), d.join("in.txt.aqr"), d.join("out.txt"));
    fs::write(&input, "Aqueous ".repeat(1000)).unwrap();

    let (mut c, lines) = start(&["-P".as_ref(), input.as_os_str()], &["passphrase"]);
    let seen = run_passphrases(&mut c, lines, &[json!("")]);
    assert!(c.wait().unwrap().success(), "{seen:?}");
    assert_eq!(events(&seen), ["passphrase", "begin", "end"]);
    assert_eq!(
        (&seen[0]["mode"], &seen[0]["confirm"], &seen[0]["path"]),
        (&json!("compress"), &json!(true), &Value::Null)
    );
    assert!(fs::read(&archive).unwrap().starts_with(&[0xf9, 0xca, 0xf2, 0xa6, 0xd0, 0x2a]));

    // asked for without -P: wrong, then right (empty)
    let args = ["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()];
    let (mut c, lines) = start(&args, &["passphrase"]);
    let seen = run_passphrases(&mut c, lines, &[json!("wrong"), json!("")]);
    assert!(c.wait().unwrap().success(), "{seen:?}");
    assert_eq!(events(&seen), ["passphrase", "passphrase", "begin", "end"]);
    assert_eq!((&seen[0]["again"], &seen[1]["again"]), (&json!(false), &json!(true)));
    assert_eq!(seen[0]["path"], archive.display().to_string());
    assert_eq!(fs::read(&out).unwrap(), fs::read(&input).unwrap());

    // given up on: an error
    let (mut c, lines) = start(&args, &["passphrase"]);
    let seen = run_passphrases(&mut c, lines, &[json!("wrong")]);
    assert!(!c.wait().unwrap().success());
    assert_eq!(events(&seen), ["passphrase", "passphrase", "error"]);

    // not taken by the frontend: an error, as without a terminal
    let (mut c, lines) = start(&args, &[]);
    let seen = run_passphrases(&mut c, lines, &[]);
    assert!(!c.wait().unwrap().success());
    assert_eq!(events(&seen), ["error"]);
    assert!(seen[0]["message"].as_str().unwrap().contains("-P"), "{seen:?}");
    fs::remove_dir_all(&d).unwrap();
}
