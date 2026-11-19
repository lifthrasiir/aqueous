//! The host functions of the Wasm host at the trust boundary: a module written by hand breaking
//! each rule of `aqr_spawn`, `aqr_result`, `aqr_join`, `aqr_take`, the channels and the part
//! functions ends in an invalid archive or a resource failure (never a panic, a hang or an
//! allocation the data cannot hold); gangs under limits; outputs not as declared; registered
//! modules unknown; the same bytes whatever the threads natively. Every application runs in the
//! sandbox too, and gives the same.

#![cfg(feature = "wasm")]

use aqr_archive::{Decoded, Encoder, Failure, Level, Runner};
use aqr_decompress::{Native, Wasm, WasmBounded, wasm_of};

/// The workers every module of these tests runs in too.
#[cfg(feature = "sandbox")]
static SANDBOX: std::sync::LazyLock<aqr_decompress::sandbox::Sandbox> = std::sync::LazyLock::new(|| {
    aqr_decompress::sandbox::Sandbox::new(env!("CARGO_BIN_EXE_aqr-decompress-worker"), [""; 0])
});

const PAGE: u64 = 1 << 16;

/// A module of `wat`, applied to one empty input for an output of `out` bytes, with `tasks` as its
/// task limits, in this process and in the sandbox, which agree.
fn apply_with(wat: &str, out: u64, limit: u64, tasks: &[u64], bound: Option<u64>) -> Decoded<Vec<Vec<u8>>> {
    let m = wat::parse_str(wat).unwrap_or_else(|e| panic!("{e}\n{wat}"));
    let bounded = WasmBounded(bound.unwrap_or(u64::MAX));
    let r = match bound {
        Some(_) => bounded.load(&m),
        None => Wasm.load(&m),
    }
    .and_then(|l| l.apply(b"", &[], &[b""], &[out], limit, tasks));
    #[cfg(feature = "sandbox")]
    if bound.is_none() {
        let s = SANDBOX.load(&m).and_then(|l| l.apply(b"", &[], &[b""], &[out], limit, tasks));
        assert!(s == r, "in the sandbox: {:?}, in this process: {:?}", s.as_ref().err(), r.as_ref().err());
    }
    r
}

/// [`apply_with`] of an output of no byte, no memory limit and one class of tasks, of no limit.
fn run(wat: &str) -> Decoded<Vec<Vec<u8>>> {
    apply_with(wat, 0, u64::MAX, &[u64::MAX], None)
}

/// A module with all the imports, and the task slots `slots` (a section's bytes), the task `task`
/// and the entry `body`, which has the locals `$h`, `$c` and `$i` and the function `$gang` (the
/// working memory as one region: the next `n` tasks of class 0). Working memory 1 page, the
/// parameters, input and output of no size.
fn module(slots: &str, task: &str, body: &str) -> String {
    format!(
        r#"(module
            (import "aqr" "aqr_memory" (memory 1))
            (import "aqr" "aqr_params" (memory i64 0))
            (import "aqr" "aqr_input0" (memory i64 0))
            (import "aqr" "aqr_output0" (memory i64 0))
            (@custom "aqr_task_slots" "{slots}")
            (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
            (import "aqr" "aqr_result" (func $result (param i32 i32)))
            (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
            (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
            (import "aqr" "aqr_chan" (func $chan (param i32) (result i32)))
            (import "aqr" "aqr_send" (func $send (param i32 i32 i32)))
            (import "aqr" "aqr_recv" (func $recv (param i32 i32 i32) (result i32)))
            (import "aqr" "aqr_close" (func $close (param i32)))
            (func $gang (param $n i32) (result i32)
                (i32.store (i32.const 0xf004) (i32.const 0x10000))
                (call $spawn (i32.const 0) (i32.const 1) (i32.const 0xf000) (i32.const 0) (local.get $n)))
            (func (export "aqr_task") (param $arg i32) (param $i i32) {task})
            (func (export "aqr_apply_whole") (param i64 i64 i64) (local $h i32) (local $c i32) (local $i i32)
                {body}))"#
    )
}

/// A module of one class of tasks, which nothing waits for (its body, then the tasks given back).
fn one(task: &str, body: &str) -> String {
    module(r"\08", task, body)
}

/// The same with a gang class.
fn gang(task: &str, body: &str) -> String {
    module(r"\81", task, body)
}

#[track_caller]
fn invalid<T: std::fmt::Debug>(r: Decoded<T>, what: &str) {
    match r {
        Err(Failure::Invalid(m)) => assert!(m.contains(what), "{m}"),
        r => panic!("{r:?}: not invalid ({what})"),
    }
}

#[track_caller]
fn resource<T: std::fmt::Debug>(r: Decoded<T>, what: &str) {
    match r {
        Err(Failure::Resource(m)) => assert!(m.contains(what), "{m}"),
        r => panic!("{r:?}: not a resource failure ({what})"),
    }
}

/// `aqr_spawn`'s records: counts and places out of the working memory, regions out of it and
/// past the 4 GiB of a memory, the count of tasks.
#[test]
fn spawn_violations() {
    // a record of the three words at 0x200
    let rec = |src: u32, len: u32, dst: u32| {
        format!(
            "(i32.store (i32.const 0x200) (i32.const {src})) (i32.store (i32.const 0x204) (i32.const {len}))
             (i32.store (i32.const 0x208) (i32.const {dst}))"
        )
    };
    let spawn = |count: i64, at: i64, n: i64| {
        format!("(drop (call $spawn (i32.const 0) (i32.const {count}) (i32.const {at}) (i32.const 0) (i32.const {n})))")
    };
    let go = |body: String| run(&one("", &body));
    // the records out of the memory: many, a few at its end, at a place past it or before it wraps
    invalid(go(spawn(0x1000_0000, 0, 1)), "out of bounds");
    invalid(go(spawn(-1, 0, 1)), "out of bounds");
    invalid(go(spawn(2, 0xfff8, 1)), "out of bounds");
    invalid(go(spawn(1, 0xfff5, 1)), "out of bounds");
    invalid(go(spawn(1, -1, 1)), "out of bounds");
    invalid(go(spawn(1, -12, 1)), "out of bounds");
    invalid(go(spawn(1, 0x10000, 1)), "out of bounds");
    // the last record that fits
    assert!(go(spawn(1, 0xfff4, 1) + "(drop (call $join (i32.const 0) (i32.const 0))) (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))").is_ok());
    // a region from out of the memory, its length alone out of it, and its sum wrapping
    invalid(go(rec(0xff00, 0x1000, 0) + &spawn(1, 0x200, 1)), "region");
    invalid(go(rec(0x10000, 1, 0) + &spawn(1, 0x200, 1)), "region");
    invalid(go(rec(0, 0xffff_ffff, 0) + &spawn(1, 0x200, 1)), "region");
    invalid(go(rec(0xffff_ffff, 0xffff_ffff, 0) + &spawn(1, 0x200, 1)), "region");
    invalid(go(rec(0xffff_fff0, 0x20, 0) + &spawn(1, 0x200, 1)), "region");
    // a region to past 4 GiB (a 32-bit memory's end), by its length, its place or the sum of both
    invalid(go(rec(0, 16, 0xffff_fff8) + &spawn(1, 0x200, 1)), "4 GiB");
    invalid(go(rec(0, 16, 0xffff_ffff) + &spawn(1, 0x200, 1)), "4 GiB");
    invalid(go(rec(0, 0x1000, 0xffff_f001) + &spawn(1, 0x200, 1)), "4 GiB");
    // the 4 GiB reached exactly is a memory the limit refuses (as no application's holds it)
    let edge = rec(0, 16, 0xffff_fff0) + &spawn(1, 0x200, 1) + "(drop (call $join (i32.const 0) (i32.const 0)))";
    resource(apply_with(&one("", &edge), 0, u64::MAX, &[1 << 24], None), "limit");
    // a later record of two that is bad fails the spawn as a whole
    let two = format!("{} (i32.store (i32.const 0x20c) (i32.const 0xffff0)) {}", rec(0, 16, 0), spawn(2, 0x200, 1));
    invalid(go(two), "region");
    // tasks of no record: their memories are the module's (one page), and with the instance's
    let none = format!(
        "{} (drop (call $join (i32.const 0) (i32.const 0))) (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))",
        spawn(0, 0, 1)
    );
    let task = "(if (i32.ne (memory.size 0) (i32.const 1)) (then unreachable))";
    assert!(run(&one(task, &none)).is_ok());
    // a count of tasks: none (a handle of no task), and more than the host allows
    assert!(go(spawn(1, 0xfff4, 0)).is_ok());
    resource(go(spawn(1, 0xfff4, -1)), "tasks");
    resource(go(spawn(1, 0xfff4, i32::MAX as i64)), "tasks");
    // a class out of the module's
    let class = |c: i32| {
        format!("(drop (call $spawn (i32.const {c}) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 1)))")
    };
    invalid(go(class(1)), "class 1");
    invalid(go(class(-1)), "class");
    invalid(go(class(0x4000_0000)), "class");
    // a task whose region reaches over the maximum the module declared for its memory
    let capped =
        one("", &(rec(0, 16, 0x20000) + &spawn(1, 0x200, 1) + "(drop (call $join (i32.const 0) (i32.const 0)))"))
            .replace("(memory 1))", "(memory 1 2))");
    let r = run(&capped);
    assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Resource(_))), "{r:?}");
}

/// `aqr_result`, `aqr_join` and `aqr_take`: places out of the memory, tasks that were never
/// spawned, joined and taken twice.
#[test]
fn result_join_take() {
    let call = |f: &str, args: &[i64]| {
        let args: String = args.iter().map(|a| format!("(i32.const {a})")).collect();
        format!("(call ${f} {args})")
    };
    // a result out of the working memory, by place, length, or both
    for (at, len) in [(0xfff0, 0x100), (0x10000, 1), (0, -1), (-1, 1), (0xffff_fff0u32 as i64, 0x20), (-1, -1)] {
        let task = call("result", &[at, len]);
        let body = "(drop (call $gang (i32.const 1))) (drop (call $join (i32.const 0) (i32.const 0)))";
        invalid(run(&one(&task, body)), "out of bounds");
    }
    // the last byte, and a result of no byte at the end of the memory
    let task = "(call $result (i32.const 0xffff) (i32.const 1))";
    let full = "(drop (call $gang (i32.const 1)))
        (if (i32.ne (call $join (i32.const 0) (i32.const 0)) (i32.const 1)) (then unreachable))
        (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 1))";
    assert!(run(&one(task, full)).is_ok());
    let task = "(call $result (i32.const 0x10000) (i32.const 0))";
    let empty = "(drop (call $gang (i32.const 1)))
        (if (i32.ne (call $join (i32.const 0) (i32.const 0)) (i32.const 0)) (then unreachable))
        (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))";
    assert!(run(&one(task, empty)).is_ok());
    // the last result given counts
    let task = "(i32.store8 (i32.const 0x100) (i32.const 7)) (call $result (i32.const 0x100) (i32.const 1))
        (i32.store8 (i32.const 0x101) (i32.const 9)) (call $result (i32.const 0x100) (i32.const 2))";
    let twice = "(drop (call $gang (i32.const 1)))
        (if (i32.ne (call $join (i32.const 0) (i32.const 0)) (i32.const 2)) (then unreachable))
        (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 2))
        (memory.copy 3 0 (i64.const 0) (i32.const 0) (i32.const 2))";
    assert_eq!(apply_with(&one(task, twice), 2, u64::MAX, &[u64::MAX], None).unwrap(), [[7, 9]]);

    // no task, no handle, an index of none, negative
    for (h, i) in [(0, 0), (1, 0), (-1, 0), (0, -1), (0x7fff_ffff, 0x7fff_ffff)] {
        let body = format!("(drop {})", call("join", &[h, i]));
        invalid(run(&one("", &body)), "of no task");
        let body = call("take", &[h, i, 0, 0]);
        invalid(run(&one("", &body)), "not joined");
    }
    let spawned = "(drop (call $gang (i32.const 2)))";
    for (h, i) in [(0, 2), (1, 0), (0, -1), (-1, 0), (0, 0x7fff_ffff)] {
        let body =
            format!("{spawned} (drop {}) (drop (call $join (i32.const 0) (i32.const 0)))", call("join", &[h, i]));
        invalid(run(&one("", &body)), "of no task");
    }
    // joined twice before taken: the same length; taken twice, joined and taken after: none
    let j = "(drop (call $join (i32.const 0) (i32.const 0)))";
    let t = "(call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))";
    let spawn_one = "(drop (call $gang (i32.const 1)))";
    assert!(run(&one("", &format!("{spawn_one} {j} {j} {t}"))).is_ok());
    invalid(run(&one("", &format!("{spawn_one} {j} {t} {t}"))), "not joined, or taken");
    invalid(run(&one("", &format!("{spawn_one} {j} {t} {j}"))), "of no task");
    // taken before joined, and a task not taken
    invalid(run(&one("", &format!("{spawn_one} {t}"))), "not joined");
    invalid(run(&one("", &format!("{spawn_one} {j}"))), "tasks not taken");
    invalid(run(&one("", spawn_one)), "tasks not taken");
    // taken out of the memory, or more bytes than the result has
    let task = "(call $result (i32.const 0x100) (i32.const 4))";
    for (dst, len) in [(0xfffe, 4), (0x10000, 1), (-1, 1), (0, -1), (0, 5), (0, 0x7fff_ffff)] {
        let body = format!("{spawn_one} {j} {}", call("take", &[0, 0, dst, len]));
        invalid(run(&one(task, &body)), "out of bounds");
    }
    // as many bytes as it has, to the very end of the memory
    let body = format!("{spawn_one} {j} {}", call("take", &[0, 0, 0xfffc, 4]));
    assert!(run(&one(task, &body)).is_ok());
}

/// The tasks' own memory violations and a trap in a task end the application: never a hang, with
/// the others waiting.
#[test]
fn task_failures() {
    // a task trapping while the instance waits for another, which waits for it
    let waits = "(drop (call $recv (i32.const 0) (i32.const 0x100) (i32.const 1)))";
    let task = format!("(if (i32.eqz (local.get $i)) (then unreachable)) {waits}");
    let body = "(drop (call $chan (i32.const 1))) (local.set $h (call $gang (i32.const 2)))
        (drop (call $join (local.get $h) (i32.const 1)))";
    invalid(run(&gang(&task, body)), "trap");
    // a violation of a task is the task's, whoever is joined
    let task = "(call $result (i32.const 0x10000) (i32.const 1))";
    let body = "(drop (call $gang (i32.const 3))) (drop (call $join (i32.const 0) (i32.const 2)))";
    invalid(run(&one(task, body)), "out of bounds");
}

/// Channels: capacities, places, channels not made, sending after closing, the ends.
#[test]
fn channel_violations() {
    let call = |f: &str, args: &[i64]| {
        let args: String = args.iter().map(|a| format!("(i32.const {a})")).collect();
        format!("(call ${f} {args})")
    };
    // capacities of none or less
    for cap in [0, -1, i32::MIN as i64] {
        invalid(run(&one("", &format!("(drop {})", call("chan", &[cap])))), "aqr_chan");
    }
    // a capacity of the greatest number of messages: a queue that grows with what is sent, the
    // memory limit counting the messages (a gang of a task sends to it, the instance takes)
    let task =
        "(i32.store8 (i32.const 0x100) (i32.const 0x42)) (call $send (i32.const 0) (i32.const 0x100) (i32.const 1))
        (call $close (i32.const 0))";
    let body = "(drop (call $chan (i32.const 0x7fffffff))) (drop (call $gang (i32.const 1)))
        (if (i32.ne (call $recv (i32.const 0) (i32.const 0x100) (i32.const 1)) (i32.const 1)) (then unreachable))
        (if (i32.ne (call $recv (i32.const 0) (i32.const 0x100) (i32.const 1)) (i32.const -1)) (then unreachable))
        (drop (call $join (i32.const 0) (i32.const 0))) (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))";
    assert!(run(&gang(task, body)).is_ok());
    // the host's number of channels is of tests/wasm.rs; a channel not made, by each function
    let made = "(drop (call $chan (i32.const 1))) (drop (call $gang (i32.const 0)))";
    for ch in [0, 1, 5, -1, 0x7fff_ffff] {
        let ch = if ch == 0 { 1 } else { ch };
        for f in [call("send", &[ch, 0, 0]), format!("(drop {})", call("recv", &[ch, 0, 0])), call("close", &[ch])] {
            invalid(run(&gang("", &format!("{made} {f}"))), "not made");
        }
    }
    // places out of the memory, for a send (the channel is the gang's, the instance its spawner)
    for (at, len) in [(0xfff0, 0x100), (0x10000, 1), (0, -1), (-1, 1), (-1, -1)] {
        let body = format!(
            "(drop (call $chan (i32.const 4))) (drop (call $gang (i32.const 0))) {}",
            call("send", &[0, at, len])
        );
        invalid(run(&gang("", &body)), "aqr_send");
    }
    // a message whose place is out of the memory in a receive: the message is the task's
    let task = "(call $send (i32.const 0) (i32.const 0x100) (i32.const 8))";
    for (dst, cap) in [(0xfffc, 8), (0x10000, 8), (-1, 8), (0x10000, -1)] {
        let body = format!(
            "(drop (call $chan (i32.const 4))) (drop (call $gang (i32.const 1)))
             (drop {}) (drop (call $join (i32.const 0) (i32.const 0)))",
            call("recv", &[0, dst, cap])
        );
        invalid(run(&gang(task, &body)), "aqr_recv");
    }
    // the largest place fitting, a message too long left queued (its length told)
    let body = "(drop (call $chan (i32.const 4))) (drop (call $gang (i32.const 1)))
        (if (i32.ne (call $recv (i32.const 0) (i32.const 0xfff8) (i32.const 8)) (i32.const 8)) (then unreachable))
        (drop (call $join (i32.const 0) (i32.const 0))) (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))";
    assert!(run(&gang(task, body)).is_ok());
    let body = "(drop (call $chan (i32.const 4))) (drop (call $gang (i32.const 1)))
        (if (i32.ne (call $recv (i32.const 0) (i32.const 0x10000) (i32.const 7)) (i32.const 8)) (then unreachable))
        (if (i32.ne (call $recv (i32.const 0) (i32.const 0x10000) (i32.const 0)) (i32.const 8)) (then unreachable))
        (drop (call $join (i32.const 0) (i32.const 0))) (call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))";
    assert!(run(&gang(task, body)).is_ok());
    // sending after closing (by the sender), closing twice, receiving after the close (none left)
    let spawned = "(drop (call $chan (i32.const 4))) (drop (call $gang (i32.const 0)))";
    invalid(
        run(&gang(
            "",
            &format!("{spawned} (call $close (i32.const 0)) (call $send (i32.const 0) (i32.const 0) (i32.const 1))"),
        )),
        "after closing",
    );
    assert!(run(&gang("", &format!("{spawned} (call $close (i32.const 0)) (call $close (i32.const 0))"))).is_ok());
    let body = format!(
        "{spawned} (call $close (i32.const 0))
         (if (i32.ne (call $recv (i32.const 0) (i32.const 0) (i32.const 1)) (i32.const -1)) (then unreachable))"
    );
    // (the instance closed it, so it is the sender: receiving takes the other end, which is its own)
    invalid(run(&gang("", &body)), "both ends");
    // a send after the receiver's gone is no error (nobody to tell): the queue is bounded by its capacity
    let body = "(drop (call $chan (i32.const 2))) (local.set $h (call $gang (i32.const 1)))
        (drop (call $join (local.get $h) (i32.const 0))) (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))
        (call $send (i32.const 0) (i32.const 0) (i32.const 1)) (call $send (i32.const 0) (i32.const 0) (i32.const 1))";
    assert!(run(&gang("", body)).is_ok());
    // a send past the capacity with nobody to receive: a deadlock, not a hang
    let body = "(drop (call $chan (i32.const 2))) (drop (call $gang (i32.const 0)))
        (call $send (i32.const 0) (i32.const 0) (i32.const 1)) (call $send (i32.const 0) (i32.const 0) (i32.const 1))
        (call $send (i32.const 0) (i32.const 0) (i32.const 1))";
    invalid(run(&gang("", body)), "deadlock");
    // a channel before the spawn of its gang, and of a plain class
    invalid(
        run(&gang("", "(drop (call $chan (i32.const 1))) (call $send (i32.const 0) (i32.const 0) (i32.const 1))")),
        "before a spawn",
    );
    invalid(run(&one("", "(drop (call $chan (i32.const 1))) (drop (call $gang (i32.const 1)))")), "no gang");
}

/// The part functions of a part module: `aqr_request`, `aqr_release` and `aqr_commit` of an index
/// not imported, out of their range, and from a task.
#[test]
fn part_violations() {
    let part = |slots: &str, task: &str, body: &str| {
        format!(
            r#"(module
                (import "aqr" "aqr_memory" (memory 1))
                (import "aqr" "aqr_params" (memory i64 0))
                (import "aqr" "aqr_input0" (memory i64 0 1))
                (import "aqr" "aqr_output0" (memory i64 0 1))
                {slots}
                (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
                (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
                (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
                (import "aqr" "aqr_request" (func $request (param i32 i64)))
                (import "aqr" "aqr_release" (func $release (param i32 i64)))
                (import "aqr" "aqr_commit" (func $commit (param i32 i64)))
                (func (export "aqr_task") (param $arg i32) (param $i i32) {task})
                (func (export "aqr_apply_part") (param i64 i64 i64) {body}))"#
        )
    };
    let input = [7u8; 100];
    let go = |wat: String, out: u64| {
        let m = wat::parse_str(&wat).unwrap_or_else(|e| panic!("{e}\n{wat}"));
        let r = Wasm.load(&m).and_then(|l| l.apply(b"", &[], &[&input], &[out], u64::MAX, &[u64::MAX]));
        #[cfg(feature = "sandbox")]
        {
            let s = SANDBOX.load(&m).and_then(|l| l.apply(b"", &[], &[&input], &[out], u64::MAX, &[u64::MAX]));
            assert!(s == r, "in the sandbox: {:?}, in this process: {:?}", s.as_ref().err(), r.as_ref().err());
        }
        r
    };
    let slots = r#"(@custom "aqr_task_slots" "\08")"#;
    let ok = "(call $request (i32.const 0) (i64.const 100)) (call $commit (i32.const 0) (i64.const 100))";
    assert!(go(part(slots, "", ok), 100).is_ok());
    // an input or an output not imported, by index (also negative: 2^32 - 1 is no index)
    for k in [1, 2, -1, i32::MIN] {
        invalid(go(part(slots, "", &format!("(call $request (i32.const {k}) (i64.const 0))")), 0), "not imported");
        invalid(go(part(slots, "", &format!("(call $release (i32.const {k}) (i64.const 0))")), 0), "not imported");
        invalid(go(part(slots, "", &format!("(call $commit (i32.const {k}) (i64.const 0))")), 0), "not imported");
    }
    // out of range: requested past the input, wide of the window, backwards; released past what
    // was requested, or before what was; committed past the output, past the window, backwards
    invalid(go(part(slots, "", "(call $request (i32.const 0) (i64.const 101))"), 0), "aqr_request");
    invalid(go(part(slots, "", "(call $request (i32.const 0) (i64.const -1))"), 0), "aqr_request");
    invalid(go(part(slots, "", "(call $request (i32.const 0) (i64.const 0x10001))"), 0), "aqr_request");
    let body = "(call $request (i32.const 0) (i64.const 50)) (call $request (i32.const 0) (i64.const 40))";
    invalid(go(part(slots, "", body), 0), "aqr_request");
    invalid(go(part(slots, "", "(call $release (i32.const 0) (i64.const 1))"), 0), "aqr_release");
    invalid(go(part(slots, "", "(call $release (i32.const 0) (i64.const -1))"), 0), "aqr_release");
    let body = "(call $request (i32.const 0) (i64.const 50)) (call $release (i32.const 0) (i64.const 30))
        (call $release (i32.const 0) (i64.const 20))";
    invalid(go(part(slots, "", body), 0), "aqr_release");
    invalid(go(part(slots, "", "(call $commit (i32.const 0) (i64.const 11))"), 10), "aqr_commit");
    invalid(go(part(slots, "", "(call $commit (i32.const 0) (i64.const -1))"), 10), "aqr_commit");
    invalid(go(part(slots, "", "(call $commit (i32.const 0) (i64.const 0x10001))"), 0x20000), "aqr_commit");
    let body = "(call $commit (i32.const 0) (i64.const 5)) (call $commit (i32.const 0) (i64.const 4))";
    invalid(go(part(slots, "", body), 10), "aqr_commit");
    // a window's worth at most between two commits
    let body = "(call $commit (i32.const 0) (i64.const 0x10000)) (call $commit (i32.const 0) (i64.const 0x30000))";
    invalid(go(part(slots, "", body), 0x30000), "aqr_commit");

    // from a task: each of them is a violation, with an index imported or not
    let spawn = "(drop (call $spawn (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 1)))
        (drop (call $join (i32.const 0) (i32.const 0)))";
    for (call, what) in [
        ("(call $request (i32.const 0) (i64.const 1))", "aqr_request in a task"),
        ("(call $release (i32.const 0) (i64.const 0))", "aqr_release in a task"),
        ("(call $commit (i32.const 0) (i64.const 0))", "aqr_commit in a task"),
        ("(call $request (i32.const 9) (i64.const 1))", "aqr_request in a task"),
    ] {
        invalid(go(part(slots, call, spawn), 0), what);
    }

    // run/4: a part module committing less than the output is, and returning
    invalid(go(part(slots, "", ok), 101), "other sizes");
    invalid(go(part(slots, "", "(call $commit (i32.const 0) (i64.const 99))"), 100), "other sizes");
    invalid(go(part(slots, "", ""), 1), "other sizes");
    // (committing nothing of an output of none is right)
    assert!(go(part(slots, "", ""), 0).is_ok());
}

/// An output a whole module did not write all of is as the memory was made: of zeros; one it
/// writes past its memory's end is a trap.
#[test]
fn output_shorter_than_declared() {
    let writes =
        |n: u32| module(r"\08", "", &format!("(memory.fill 3 (i64.const 0) (i32.const 0x55) (i64.const {n}))"));
    let r = apply_with(&writes(3), 8, u64::MAX, &[u64::MAX], None).unwrap();
    assert_eq!(r, [[0x55, 0x55, 0x55, 0, 0, 0, 0, 0]]);
    let r = apply_with(&writes(70_000), 8, u64::MAX, &[u64::MAX], None);
    invalid(r, "trap");
}

/// A gang of tasks that all wait on channels nobody sends to: a deadlock the host finds, however
/// many are waiting, with the instance joined or not.
#[test]
fn gang_deadlocks() {
    let recv = "(drop (call $recv (local.get $i) (i32.const 0x100) (i32.const 1)))";
    for n in [1, 2, 8, 64] {
        let chans = (0..n).map(|_| "(drop (call $chan (i32.const 1)))").collect::<String>();
        // the instance joins them
        let body =
            format!("{chans} (drop (call $gang (i32.const {n}))) (drop (call $join (i32.const 0) (i32.const 0)))");
        let wat = module(r"\88", recv, &body);
        invalid(run(&wat), "deadlock");
        // the instance waits on a channel of its own, which nobody sends to
        let body = format!(
            "{chans} (drop (call $gang (i32.const {n}))) (drop (call $recv (i32.const 0) (i32.const 0) (i32.const 1)))"
        );
        invalid(run(&module(r"\88", "", &body)), "deadlock");
    }
    // two tasks, each waiting for the other's message
    let task = "(drop (call $recv (local.get $i) (i32.const 0x100) (i32.const 1)))
        (call $send (i32.sub (i32.const 1) (local.get $i)) (i32.const 0x100) (i32.const 1))";
    let body = "(drop (call $chan (i32.const 1))) (drop (call $chan (i32.const 1)))
        (drop (call $gang (i32.const 2))) (drop (call $join (i32.const 0) (i32.const 1)))";
    invalid(run(&module(r"\88", task, body)), "deadlock");
}

/// A gang's limit counts its messages: a task sending more than the limit lets queued, however
/// large the capacity of the channel, is over the limit; and empty messages too, which hold no
/// bytes but a queue of their own.
#[test]
fn gang_message_limits() {
    // a task sending `n` messages of `len` bytes to a channel of a capacity of 2^31-1, which the
    // instance never takes before they are all sent
    let sender = |n: u32, len: u32| {
        format!(
            "(local.set $i (i32.const 0))
             (loop $next (if (i32.lt_u (local.get $i) (i32.const {n})) (then
                (call $send (i32.const 0) (i32.const 0x100) (i32.const {len}))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $next))))
             (call $close (i32.const 0))"
        )
    };
    let body = "(drop (call $chan (i32.const 0x7fffffff))) (local.set $h (call $gang (i32.const 1)))
        (drop (call $join (local.get $h) (i32.const 0)))
        (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))";
    let wat = |n, len| gang(&sender(n, len), body);
    // a task of a page, its messages in the rest of the limit
    let limit = PAGE + 1000;
    let run = |n, len| apply_with(&wat(n, len), 0, u64::MAX, &[limit], None);
    // (the instance joins the task, which ends having sent them all: they fit, or do not)
    assert!(run(10, 100).is_ok());
    assert!(run(1000, 1).is_ok());
    resource(run(1001, 1), "limit");
    resource(run(11, 100), "limit");
    resource(run(1, 1001), "limit");
    // messages of no byte: the queue must not grow without a limit
    let r = run(50_000, 0);
    resource(r, "limit");
}

/// The task limit 0 and the limits too small for a task's memory: a resource failure before the
/// task runs, in a class that is a gang or not; and no task of such a class is no failure.
#[test]
fn task_limit_zero() {
    let spawn = |n: u32| format!("(drop (call $gang (i32.const {n}))) (drop (call $join (i32.const 0) (i32.const 0)))");
    let take = "(call $take (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))";
    for slots in [r"\08", r"\81"] {
        let wat = module(slots, "", &format!("{} {take}", spawn(1)));
        resource(apply_with(&wat, 0, u64::MAX, &[0], None), "limit");
        resource(apply_with(&wat, 0, u64::MAX, &[PAGE - 1], None), "limit");
        assert!(apply_with(&wat, 0, u64::MAX, &[PAGE], None).is_ok());
        // no task, no memory of a class that holds none
        let none = module(slots, "", "(drop (call $gang (i32.const 0)))");
        assert!(apply_with(&none, 0, u64::MAX, &[0], None).is_ok());
    }
    // the instance's limit of none: the module's own memories do not fit
    let nothing = module(r"\08", "", "");
    resource(apply_with(&nothing, 0, 0, &[0], None), "limit");
    resource(apply_with(&nothing, 0, PAGE - 1, &[0], None), "limit");
    // limits not one for each class
    invalid(apply_with(&nothing, 0, u64::MAX, &[], None), "task limits");
    invalid(apply_with(&nothing, 0, u64::MAX, &[0, 0], None), "task limits");
}

/// A gang larger than its class's slots, the concurrency lowered to 1 by a bound: the gang is one
/// unit and starts together (the first task sends to the channels of all the others, which wait).
#[test]
fn gang_beyond_slots_at_one() {
    let n = 9;
    let task = "(if (i32.eqz (local.get $i))
            (then (local.set $arg (i32.const 1))
                (loop $next (if (i32.lt_u (local.get $arg) (i32.const 9)) (then
                    (call $send (local.get $arg) (i32.const 0x100) (i32.const 1))
                    (local.set $arg (i32.add (local.get $arg) (i32.const 1)))
                    (br $next)))))
            (else
                (if (i32.ne (call $recv (local.get $i) (i32.const 0x100) (i32.const 1)) (i32.const 1)) (then unreachable))))";
    let mut body = (0..n).map(|_| "(drop (call $chan (i32.const 1)))").collect::<String>();
    body += &format!("(local.set $h (call $gang (i32.const {n})))");
    for i in 0..n {
        body += &format!(
            "(drop (call $join (local.get $h) (i32.const {i}))) (call $take (local.get $h) (i32.const {i}) (i32.const 0) (i32.const 0))"
        );
    }
    // (a class of 9 slots, a gang)
    let wat = module(r"\89", task, &body);
    // a gang of 9 tasks of a page, and the messages
    let gang_limit = 9 * PAGE + 64;
    let all = apply_with(&wat, 0, 8 * PAGE, &[gang_limit], None);
    assert!(all.is_ok(), "{all:?}");
    // concurrency 1 by a bound of one unit of the class
    let bound = 8 * PAGE + gang_limit;
    let one = apply_with(&wat, 0, 8 * PAGE, &[gang_limit], Some(bound));
    assert!(one.is_ok(), "{one:?}");
    // a bound short of one unit: a failure before it starts
    resource(apply_with(&wat, 0, 8 * PAGE, &[gang_limit], Some(bound - 1)), "bound");
}

/// A shorthand module block of a hash no registry holds: a resource failure, whichever runs it.
#[test]
fn unknown_registered_hash() {
    use aqr_archive::Wasm as W;
    let d = vec![0x42u8; 5000];
    let f = aqr_raw::formula::encode(&wav(&d), 1).unwrap();
    fn unknown(m: aqr_formula::Module) -> Option<W> {
        let k = aqr_decompress::built(m)?;
        let _ = k;
        // (the classes the module has, as an archive of the module would declare)
        Some(W::Registered {
            hash: [0x5a; 32],
            classes: aqr_archive::class_slots(&aqr_decompress::built(m)?.read_wasm().ok()?).len(),
        })
    }
    let c = Encoder::new(Level::Zstd(3)).with_modules(unknown).encode(&f, None).unwrap();
    let want = |r: Decoded<Vec<u8>>, who: &str| match r {
        Err(Failure::Resource(m)) => assert!(m.contains("not known"), "{who}: {m}"),
        r => panic!("{who}: {:?}", r.map(|v| v.len())),
    };
    want(aqr_archive::decode(&c, &Native::default()), "native");
    want(aqr_archive::decode(&c, &Wasm), "wasm");
    #[cfg(feature = "sandbox")]
    {
        let sandboxed = aqr_decompress::sandbox::Sandboxed { native: Native::default(), sandbox: &SANDBOX };
        want(aqr_archive::decode(&c, &sandboxed), "sandboxed");
    }
    // the same archive naming its modules by their hashes of the registry: run by each, alike
    fn shorthand(m: aqr_formula::Module) -> Option<W> {
        let k = aqr_decompress::built(m)?;
        Some(W::Registered { hash: k.sha512_256, classes: aqr_archive::class_slots(&k.read_wasm().ok()?).len() })
    }
    let c = Encoder::new(Level::Zstd(3)).with_modules(shorthand).encode(&f, None).unwrap();
    let w = wav(&d);
    assert!(aqr_archive::decode(&c, &Native::default()).unwrap() == w);
    assert!(aqr_archive::decode(&c, &Wasm).unwrap() == w);
    #[cfg(feature = "sandbox")]
    {
        let sandboxed = aqr_decompress::sandbox::Sandboxed { native: Native::default(), sandbox: &SANDBOX };
        assert!(aqr_archive::decode(&c, &sandboxed).unwrap() == w);
    }
    // and a formula whose root is no application at all decodes with any runner
    let mut plain = aqr_formula::Formula::default();
    let a = plain.push("a", d.clone());
    plain.root = a;
    let c = Encoder::new(Level::Zstd(3)).with_modules(wasm_of).encode(&plain, None).unwrap();
    assert!(aqr_archive::decode(&c, &Native::default()).unwrap() == d);
}

/// A 16-bit mono WAV of `d` repeated, for aqr-raw's PCM.
fn wav(d: &[u8]) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + d.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&[1, 0, 1, 0]);
    w.extend_from_slice(&44100u32.to_le_bytes());
    w.extend_from_slice(&(44100u32 * 2).to_le_bytes());
    w.extend_from_slice(&[2, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(d.len() as u32).to_le_bytes());
    w.extend_from_slice(d);
    w
}

/// `f` with the limits of its application declared: `memory` and `classes` task limits of `task`.
fn declared(mut f: aqr_formula::Formula, memory: u64, task: u64) -> aqr_formula::Formula {
    let aqr_formula::Node::Concat(parts) = &mut f.root else { panic!("not a concatenation") };
    let a = parts.iter_mut().find_map(|n| if let aqr_formula::Node::Apply(a) = n { Some(a) } else { None });
    let a = a.expect("an application");
    let classes = aqr_archive::class_slots(&aqr_decompress::built(a.module).unwrap().read_wasm().unwrap()).len();
    (a.memory_limit, a.task_limits) = (memory, vec![task; classes]);
    f
}

/// A stereo 16-bit WAV of `n` frames of noisy tones, for aqr-raw's PCM pipeline.
fn tones(n: u32) -> Vec<u8> {
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

/// The limits an archive declares are held to by every runner alike: over the decoder's bound of
/// 512 MiB (`--application-bound`), the application is refused before it is applied; under what
/// the module needs, it fails as a resource failure, natively as well as as WebAssembly.
#[test]
fn declared_limits_by_runner() {
    let d = tones(100_000);
    let f = aqr_raw::formula::encode(&d, 1).unwrap();
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let run_all = |c: &[u8], what: &str, text: &str| {
        let mut rs =
            vec![("native", aqr_archive::decode(c, &Native::default())), ("wasm", aqr_archive::decode(c, &Wasm))];
        #[cfg(feature = "sandbox")]
        {
            let sandboxed = aqr_decompress::sandbox::Sandboxed { native: Native::default(), sandbox: &SANDBOX };
            rs.push(("sandboxed", aqr_archive::decode(c, &sandboxed)));
            rs.push(("worker", aqr_archive::decode(c, &*SANDBOX)));
        }
        for (who, r) in rs {
            match r {
                Err(Failure::Resource(m)) => assert!(m.contains(text), "{what}, {who}: {m}"),
                r => panic!("{what}, {who}: {:?}", r.map(|v| v.len())),
            }
        }
    };
    // 512 MiB and a byte
    let c = enc.encode(&declared(f.clone(), (512 << 20) + 1, 0), None).unwrap();
    run_all(&c, "over the bound", "--application-bound");
    let c = enc.encode(&declared(f.clone(), 256 << 20, (256 << 20) + 1), None).unwrap();
    run_all(&c, "over the bound by its tasks", "--application-bound");
    // the working memory and the files of an application that do not fit its limit
    let c = enc.encode(&declared(f.clone(), 1 << 16, 1 << 16), None).unwrap();
    run_all(&c, "under the plan", "limit");
    // and what is within it decodes
    let c = enc.encode(&declared(f, 256 << 20, 64 << 20), None).unwrap();
    assert!(aqr_archive::decode(&c, &Native::default()).unwrap() == d);
    assert!(aqr_archive::decode(&c, &Wasm).unwrap() == d);
}

/// What the gang modules (the pipelines of FLAC and PCM, ProRes's stripes) decode to natively does
/// not depend on the threads: the same bytes at 1, 2, 7 and 16, and they are the file's.
#[test]
fn native_threads_do_not_matter() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut files = Vec::new();
    for t in ["s16_junk.flac", "s32.flac"] {
        let d = fixture(&format!("codecs/aqr-flac/tests/data/{t}"));
        files.push((t.to_string(), aqr_flac::formula::encode(&d, 1).unwrap(), d));
    }
    let d = tones(300_000);
    files.push(("pcm".into(), aqr_raw::formula::encode(&d, 1).unwrap(), d));
    // (frames of 720x480 in stripes of tasks)
    let mut d = b"   wide".to_vec();
    for i in 0..4 {
        d.extend_from_slice(&aqr_prores::synth::synthetic(720, 480, 2, 0, i));
    }
    let f = aqr_prores::formula::encode_with(&d, 2, aqr_prores::formula::CHUNK, Some(16)).unwrap();
    files.push(("prores".into(), f, d));
    for (name, f, d) in files {
        let c = enc.encode(&f, None).unwrap();
        for threads in [1, 2, 7, 16] {
            let r = aqr_archive::decode(&c, &Native { threads });
            assert!(r.as_ref().is_ok_and(|r| *r == d), "{name} at {threads} threads: {:?}", r.err());
        }
    }
}
