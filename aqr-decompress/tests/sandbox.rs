//! The sandbox's workers: locked down, and failing as resource failures where they cannot run.
//! (That modules give in them what they give in this process is of tests/wasm.rs.)

#![cfg(feature = "sandbox")]

use aqr_decompress::sandbox::Sandbox;
use aqr_formula::aqr::{Failure, Runner};

const WORKER: &str = env!("CARGO_BIN_EXE_aqr-decompress-worker");

/// A module of the profile doing nothing.
fn nothing() -> Vec<u8> {
    wat::parse_str(
        r#"(module (import "aqr" "aqr_memory" (memory 0)) (func (export "aqr_apply_whole") (param i64 i64)))"#,
    )
    .unwrap()
}

/// A worker reads no file, writes none, starts no process and connects nowhere.
#[test]
fn locked_down() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // what this process can do
    std::fs::read(WORKER).unwrap();
    std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();

    let tried = Sandbox::new(WORKER, [""; 0]).probe(WORKER, port).unwrap();
    let _ = std::fs::remove_file(format!("{WORKER}.probe"));
    assert_eq!(tried.len(), 4, "{tried:?}");
    for t in &tried {
        assert!(!t.ends_with(": ok"), "{tried:?}");
    }
}

/// A worker that cannot be started is a resource failure, as one that ends unexpectedly.
#[test]
fn not_started() {
    let s = Sandbox::new(format!("{WORKER}.missing"), [""; 0]);
    assert!(matches!(s.load(&nothing()), Err(Failure::Resource(m)) if m.contains("not started")));
    // a program that is not a worker (exits at once)
    let s = Sandbox::new(if cfg!(windows) { r"C:\Windows\System32\whoami.exe" } else { "/bin/true" }, [""; 0]);
    assert!(matches!(s.load(&nothing()), Err(Failure::Resource(_))));
}

/// The worker program, run by anything but a sandbox, refuses to serve (it would not be
/// locked down). On Linux it locks itself down, wherever it is started.
#[cfg(windows)]
#[test]
fn refuses_unsandboxed() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut c = Command::new(WORKER).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = c.stdin.take().unwrap();
    input.write_all(b"AQRSBX\x00\x01").unwrap();
    input.write_all(&1u64.to_le_bytes()).unwrap();
    let mut out = Vec::new();
    c.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(out.is_empty());
    assert_eq!(c.wait().unwrap().code(), Some(1));
}
