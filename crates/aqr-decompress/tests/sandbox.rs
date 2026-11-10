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

/// A module spinning for a while (`n` rounds of a loop).
fn spinning(n: u64) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module (import "aqr" "aqr_memory" (memory 0))
            (func (export "aqr_apply_whole") (param i64 i64) (local $i i64)
              (loop $l
                (local.set $i (i64.add (local.get $i) (i64.const 1)))
                (br_if $l (i64.lt_u (local.get $i) (i64.const {n}))))))"#
    ))
    .unwrap()
}

/// `--stats` sees what a worker does: its CPU time is thread-seconds outside the pool, at work
/// for the item and under the spans of the thread that applied the module.
#[test]
fn work_is_traced() {
    use aqr_common::trace;
    trace::enable();
    let rec = trace::record().unwrap();
    let s = Sandbox::new(WORKER, [""; 0]);
    let l = s.load(&spinning(1 << 31)).unwrap();
    let start = std::time::Instant::now();
    {
        let _item = trace::item(Some(0));
        let _span = trace::span("test");
        l.apply(&[], &[], &[], &[1], 1 << 20, &[]).unwrap();
    }
    let wall = start.elapsed().as_secs_f64();
    let t = rec.finish();
    let mut busy = 0.0;
    for (i, smp) in t.samples.iter().enumerate() {
        let dt = t.samples.get(i + 1).map_or(0.0, |n| n.at - smp.at);
        for (k, &(stack, item)) in smp.threads.iter().enumerate() {
            let work = t.stacks.get(stack as usize).is_some_and(|st| {
                st.last().is_some_and(|&l| !t.labels[l as usize].1)
                    && st.iter().any(|&l| t.labels[l as usize].0 == "test")
            });
            if !t.threads[k].1 && item == 0 && work && t.threads[k].0 != "work_is_traced" {
                busy += dt;
            }
        }
    }
    assert!(wall > 0.1, "the module spun for {wall} s");
    assert!(busy >= 0.5 * wall, "{busy} thread-seconds outside the pool of a run of {wall} s");
}
