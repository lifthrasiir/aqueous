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
