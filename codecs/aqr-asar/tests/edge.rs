//! Edge cases: JSON deeper than the reader goes, offsets past 2^53, block sizes of 0, keys
//! twice, claimed lengths the inputs cannot hold, damaged archives.

use std::time::Duration;

use aqr_asar::formula;
use aqr_asar::layout;
use aqr_common::rng::noise;
use aqr_formula::Node;
use aqr_testkit::{Refusing, case, damaged, most, within};
use sha2::{Digest, Sha256};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn archive(json: &str, data: &[u8]) -> Vec<u8> {
    let mut d = layout::prefix(json.len()).to_vec();
    d.extend_from_slice(json.as_bytes());
    d.resize(d.len() + layout::pad_to(json.len(), 4), 0);
    d.extend_from_slice(data);
    d
}

fn eval(f: &aqr_formula::Formula, n: &Node) -> Vec<u8> {
    match n {
        &Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| eval(f, n)).collect(),
        _ => panic!("{n:?}"),
    }
}

/// The module's inputs of the formula of `d`, if it has one.
fn inputs(d: &[u8]) -> Option<Vec<Vec<u8>>> {
    let f = formula::encode(d, 1)?;
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    Some(a.inputs.iter().map(|n| eval(&f, n)).collect())
}

/// What the module gives for the formula of `d`, if it has one.
fn rebuilt(d: &[u8]) -> Option<Vec<u8>> {
    let ins = inputs(d)?;
    let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
    Some(formula::decode(&[], &refs, d.len() as u64, 1).unwrap())
}

fn check(d: &[u8]) {
    case("asar");
    if let Some(r) = rebuilt(d) {
        assert!(r == d);
    }
    assert!(most() < 256 << 20);
}

/// JSON nested past the reader's depth is not read (and does not overflow the stack): the
/// archive is not taken; nested to just within it is.
#[test]
fn deep_json() {
    let a = noise(100);
    for depth in [900, 990, 1001, 5000, 200_000] {
        let json = format!(
            r#"{{"files":{{"a":{{"size":100,"offset":"0","x":{}{}}}}}}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let d = archive(&json, &a);
        within(LIMIT, || {
            assert_eq!(formula::encode(&d, 1).is_some(), depth <= 990, "depth {depth}");
            check(&d);
        })
        .unwrap();
    }
    // directories nested deep (each a `files` object)
    for depth in [100, 400, 2000] {
        let json =
            format!(r#"{}"f":{{"size":100,"offset":"0"}}{}"#, r#"{"files":{"d":"#.repeat(depth), "}}".repeat(depth));
        let d = archive(&json, &a);
        within(LIMIT, || check(&d)).unwrap();
    }
}

/// Offsets and sizes past 2^53 or 2^64 are not the data's: none is taken, or the archive not
/// at all.
#[test]
fn huge_numbers() {
    let a = noise(100);
    for (size, offset) in [
        ("100", "9007199254740993"),
        ("9007199254740993", "0"),
        ("100", "18446744073709551615"),
        ("18446744073709551615", "1"),
        ("100", "18446744073709551616"),
        ("99999999999999999999999999", "0"),
    ] {
        let d = archive(&format!(r#"{{"files":{{"a":{{"size":{size},"offset":"{offset}"}}}}}}"#), &a);
        assert!(formula::encode(&d, 1).is_none(), "{size} {offset}");
        // (next to a file that is there)
        let d = archive(
            &format!(r#"{{"files":{{"b":{{"size":100,"offset":"0"}},"a":{{"size":{size},"offset":"{offset}"}}}}}}"#),
            &a,
        );
        assert!(rebuilt(&d).expect("a formula") == d);
        assert_eq!(formula::files(&d).len(), 1);
    }
}

/// A block size of 0 (or one not a whole number) leaves the integrity as it is: no hole, no
/// division by zero; a hole of block size 0 in a header is corrupt.
#[test]
fn block_size_zero() {
    let a = noise(100);
    let h: String = Sha256::digest(&a).iter().map(|b| format!("{b:02x}")).collect();
    for bs in ["0", "-1", "1.5", "00", "\"4\"", "18446744073709551616"] {
        let json = format!(
            r#"{{"files":{{"a":{{"size":100,"offset":"0","integrity":{{"algorithm":"SHA256","hash":"{h}","blockSize":{bs},"blocks":["{h}"]}}}}}}}}"#
        );
        let d = archive(&json, &a);
        assert!(rebuilt(&d).expect("a formula") == d, "{bs}");
    }
    let header = b"{\"files\":{\"a\":{\"size\":\0\x01,\"offset\":\"\0\x02\",\"integrity\":{\"blocks\":[\0\x04\x00]}}}}";
    let mut index = vec![1, 0, 100];
    index.extend_from_slice(&[0; 16]);
    assert!(layout::build(&a, header, &index, 1000).is_err());
}

/// A key twice in an object: the archive is rebuilt either way (or not taken).
#[test]
fn duplicate_keys() {
    let a = noise(3000);
    for json in [
        r#"{"files":{"a":{"size":100,"offset":"0"},"a":{"size":200,"offset":"100"}}}"#,
        r#"{"files":{"a":{"size":100,"size":200,"offset":"0","offset":"5"}}}"#,
        r#"{"files":{"a":{"size":100,"offset":"0"}},"files":{"b":{"size":200,"offset":"100"}}}"#,
        r#"{"files":{"a":{"size":100,"offset":"0","integrity":{"hash":"x"},"integrity":{"hash":"y"}}}}"#,
        r#"{"files":{"a":{"size":100,"offset":"0","files":{}}}}"#,
    ] {
        let d = archive(json, &a);
        if let Some(r) = rebuilt(&d) {
            assert!(r == d, "{json}");
        }
    }
}

/// The module's claimed length is not allocated before the data shows it can be reached.
#[test]
fn claimed_length() {
    let a = noise(100);
    let d = archive(r#"{"files":{"a":{"size":100,"offset":"0"}}}"#, &a);
    let ins = inputs(&d).unwrap();
    let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
    for len in [d.len() as u64 + 1, 1 << 30, 1 << 40, 1 << 62, u64::MAX] {
        case("len");
        assert!(formula::decode(&[], &refs, len, 1).is_err(), "{len}");
        assert!(most() < 256 << 20, "{len}");
    }
}

#[test]
fn damaged_archives() {
    let a = noise(300);
    let json = r#"{"files":{"a":{"size":100,"offset":"0"},"d":{"files":{"b":{"size":200,"offset":"100","integrity":{"algorithm":"SHA256","hash":"x","blockSize":64,"blocks":["y"]}}}}}}"#;
    let d = archive(json, &a);
    damaged(&d, 100, 120, LIMIT, check);
}
