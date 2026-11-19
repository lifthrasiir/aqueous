//! The sandbox's workers: locked down, and failing as resource failures where they cannot run.
//! (That modules give in them what they give in this process is of tests/wasm.rs.)

#![cfg(feature = "sandbox")]

use aqr_archive::{Failure, Runner};
use aqr_decompress::sandbox::Sandbox;

const WORKER: &str = env!("CARGO_BIN_EXE_aqr-decompress-worker");

/// Held for reading by the tests that run workers and for writing by those that kill them (all
/// the workers of the process) or set the bound of the application memories (the process's).
static WORKERS: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn workers() -> std::sync::RwLockReadGuard<'static, ()> {
    WORKERS.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn alone() -> std::sync::RwLockWriteGuard<'static, ()> {
    WORKERS.write().unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
    let _w = workers();
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
    let _w = workers();
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

/// A module spinning for a million rounds for each byte of the parameters.
fn spinning_by_params() -> Vec<u8> {
    wat::parse_str(
        r#"(module (import "aqr" "aqr_memory" (memory 0))
            (func (export "aqr_apply_whole") (param $n i64) (param i64) (local $i i64)
              (loop $l
                (local.set $i (i64.add (local.get $i) (i64.const 1)))
                (br_if $l (i64.lt_u (local.get $i) (i64.mul (local.get $n) (i64.const 1048576)))))))"#,
    )
    .unwrap()
}

/// Kills every worker of this process (its child processes).
fn kill_workers() {
    use std::process::Command;
    let me = std::process::id();
    let name = std::path::Path::new(WORKER).file_name().unwrap().to_string_lossy();
    let ok = if cfg!(windows) {
        let script = format!(
            "Get-CimInstance Win32_Process -Filter \"ParentProcessId={me} AND Name='{name}'\" | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}"
        );
        Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output()
    } else {
        Command::new("pkill").args(["-KILL", "-P", &me.to_string()]).output()
    };
    assert!(
        ok.as_ref().is_ok_and(|o| o.status.success()),
        "{:?}",
        ok.map(|o| format!("{} {}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    );
}

/// A worker dying (killed) while it applies a module, or while idle: the application is a resource
/// failure that says so, never a wait for ever, and the same sandbox goes on with a worker of its own
/// making, for this module and others.
#[test]
fn worker_dies() {
    let _alone = alone();
    let s = Sandbox::new(WORKER, [""; 0]);
    let l = s.load(&spinning_by_params()).unwrap();
    // (a few seconds of spinning when left alone)
    let params = vec![0u8; 16 << 10];
    let r = std::thread::scope(|t| {
        let busy = t.spawn(|| l.apply(&params, &[], &[], &[1], 1 << 20, &[]));
        std::thread::sleep(std::time::Duration::from_millis(1500));
        kill_workers();
        busy.join().unwrap()
    });
    assert!(matches!(&r, Err(Failure::Resource(m)) if m.contains("worker ended")), "{r:?}");
    // the same sandbox, the module loaded and another, and with none to spin
    assert_eq!(l.apply(&[], &[], &[], &[1], 1 << 20, &[]).unwrap(), [[0]]);
    let n = s.load(&nothing()).unwrap();
    assert_eq!(n.apply(&[], &[], &[], &[1], 1 << 20, &[]).unwrap(), [[0]]);
    // a worker that died idle: the application it was given fails as ended, and the next is served
    kill_workers();
    let first = n.apply(&[], &[], &[], &[1], 1 << 20, &[]);
    assert!(matches!(&first, Err(Failure::Resource(m)) if m.contains("worker ended")) || first.is_ok(), "{first:?}");
    assert_eq!(n.apply(&[], &[], &[], &[1], 1 << 20, &[]).unwrap(), [[0]]);
}

/// A 16-bit stereo WAV file of noisy tones, for aqr-raw's PCM: a module with tasks.
fn wav() -> Vec<u8> {
    let n: u32 = 200_000;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + 4 * n).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&[1, 0, 2, 0]);
    d.extend_from_slice(&44100u32.to_le_bytes());
    d.extend_from_slice(&(44100u32 * 4).to_le_bytes());
    d.extend_from_slice(&[4, 0, 16, 0]);
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(4 * n).to_le_bytes());
    let mut r = 1u32;
    for i in 0..n {
        for c in 0..2 {
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = ((i as f64 * (0.01 + 0.003 * c as f64)).sin() * 9000.0) as i32 + (r >> 26) as i32 - 32;
            d.extend_from_slice(&(x as i16).to_le_bytes());
        }
    }
    d
}

/// The bound of the application memories (`-M`) is held to as WebAssembly, in this process and in
/// a worker, and not natively: a known module runs natively by the memory limit it declares
/// and its plan alone, however small the bound (its decoding is the same bytes).
#[test]
fn application_bound_by_runner() {
    use aqr_decompress::{Native, Wasm, wasm_of};
    struct Unbound;
    impl Drop for Unbound {
        fn drop(&mut self) {
            aqr_decompress::set_application_bound(u64::MAX);
        }
    }
    let _alone = alone();
    let want = wav();
    let mut f = aqr_raw::formula::encode(&want, 1).unwrap();
    // (limits declared, as aqr-compress does: 256 MiB, and 64 MiB for each class of tasks)
    let aqr_formula::Node::Concat(parts) = &mut f.root else { panic!("not a concatenation") };
    let a = parts.iter_mut().find_map(|n| if let aqr_formula::Node::Apply(a) = n { Some(a) } else { None });
    let a = a.expect("an application");
    let classes = aqr_archive::class_slots(&aqr_decompress::built(a.module).unwrap().read_wasm().unwrap()).len();
    assert!(classes > 0);
    (a.memory_limit, a.task_limits) = (256 << 20, vec![64 << 20; classes]);
    let c = aqr_archive::Encoder::new(aqr_archive::Level::Zstd(3)).with_modules(wasm_of).encode(&f, None).unwrap();
    let s = Sandbox::new(WORKER, [""; 0]);
    let sandboxed = aqr_decompress::sandbox::Sandboxed { native: Native::default(), sandbox: &s };
    let _unbound = Unbound;
    for bound in [1, 1 << 20] {
        aqr_decompress::set_application_bound(bound);
        let native = aqr_archive::decode(&c, &Native::default());
        let mixed = aqr_archive::decode(&c, &sandboxed);
        let wasm = aqr_archive::decode(&c, &Wasm);
        let worker = aqr_archive::decode(&c, &s);
        assert!(native.as_ref().is_ok_and(|n| *n == want), "native, bound {bound}: {:?}", native.err());
        assert!(mixed.as_ref().is_ok_and(|n| *n == want), "sandboxed, bound {bound}: {:?}", mixed.err());
        for (who, r) in [("wasm", wasm), ("worker", worker)] {
            assert!(
                matches!(&r, Err(Failure::Resource(m)) if m.contains("bound")),
                "{who}, bound {bound}: {:?}",
                r.map(drop)
            );
        }
    }
    aqr_decompress::set_application_bound(u64::MAX);
    let (wasm, worker) = (aqr_archive::decode(&c, &Wasm), aqr_archive::decode(&c, &s));
    assert!(wasm.is_ok_and(|w| w == want));
    assert!(worker.is_ok_and(|w| w == want));
}
