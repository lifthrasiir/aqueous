//! Edge cases of the text: huge indices and numbers, mixed line ends, portions cut inside
//! them, and damaged streams.

use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};
use aqr_obj::codec::Options;
use aqr_obj::formula::{self, MODULE};
use aqr_testkit::Refusing;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 28);

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// Round trips with verification off (the coded path itself), in blocks and portions of
/// small sizes, on one thread and on several.
fn rt(src: &[u8]) {
    for (block, portion, threads) in [(1 << 10, 4 << 20, 1), (4 << 20, 4 << 20, 1), (1 << 10, 1 << 12, 3), (64, 100, 2)]
    {
        let opt = Options { verify: false, block, portion, threads, ..Default::default() };
        let f = formula::encode_with(src, &opt).unwrap_or_else(|| panic!("block {block}: no formula"));
        assert_eq!(eval(&f, threads).unwrap(), src, "block {block}, portion {portion}");
    }
    // With verification on, whatever it stores whole must come back as well.
    if let Some(f) = formula::encode(src, 1) {
        assert_eq!(eval(&f, 1).unwrap(), src);
    }
}

#[test]
fn indices_past_32_bits() {
    let mut s = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    for i in [
        "2147483647",
        "2147483648",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "123456789012345678901234567890",
        "-2147483648",
        "-2147483649",
        "-4294967296",
        "-9223372036854775808",
        "-9223372036854775809",
        "-18446744073709551616",
        "00000000000000000000000000001",
    ] {
        s += &format!("f 1 2 {i}\nf {i}/{i} 2/{i} 3/{i}\nf 1//{i} 2//{i} {i}//1\nl {i} 1\n");
    }
    rt(s.as_bytes());
    // the same among many lines, so that the model has its state
    let mut t = String::new();
    for i in 0..400u64 {
        t += &format!("v {i} 1 2\nf {} {} {}\n", i, i * 5_000_000_011, u64::MAX - i);
    }
    rt(t.as_bytes());
}

#[test]
fn numbers_of_many_digits_and_exponents() {
    let mut s = String::new();
    for n in [
        "1e400",
        "-1e400",
        "1e-400",
        "1e99999999999999999999",
        "1e-99999999999999999999",
        "1e+2147483648",
        "0e999999",
        "0.0000000000000000000000000000000000000001",
        "123456789012345678901234567890.123456789012345678901234567890",
        "99999999999999999999",
        "100000000000000000000",
        "18446744073709551616",
        "-0.00000000000000000000",
        "1.",
        ".e5",
        "1e",
        "1e+",
        "1.5E-",
        "0x10",
        "1_000",
        "١٢٣",
        "infinity",
        "-nan",
        "1.7976931348623157e308",
        "1.7976931348623159e308",
        "4.9e-324",
        "2.4e-324",
    ] {
        s += &format!("v {n} {n} {n}\nvn {n} 0 1\nvt {n} {n}\n");
    }
    rt(s.as_bytes());
    // 20+ digit numbers among regular ones, in a column
    let mut t = String::new();
    for i in 0..300u64 {
        let wide = if i % 5 == 0 { format!("{i}{}", "7".repeat(25)) } else { format!("{i}.25") };
        t += &format!("v {wide} {i}.5 {}\n", i % 3);
    }
    rt(t.as_bytes());
}

#[test]
fn mixed_line_ends() {
    let mut s = Vec::new();
    for i in 0..300 {
        let end: &[u8] = [&b"\n"[..], b"\r\n", b"\r", b"\n\r", b"\r\r\n", b" \r\n", b"\t\n"][i % 7];
        s.extend_from_slice(format!("v {i}.5 {} 3", i * 2).as_bytes());
        s.extend_from_slice(end);
        if i % 3 == 0 {
            s.extend_from_slice(format!("f {} {} {}", i + 1, i + 2, i + 3).as_bytes());
            s.extend_from_slice(end);
        }
    }
    // no final line end, a lone CR last, and NUL in the middle of a line
    rt(&s);
    s.extend_from_slice(b"v 1 2 3");
    rt(&s);
    s.extend_from_slice(b"\r");
    rt(&s);
    s.extend_from_slice(b"\nv 1\0 2 3\n\0\0\n");
    rt(&s);
}

/// A portion boundary falling inside a CR LF, after a lone CR, and inside a number.
#[test]
fn portion_boundaries_in_line_ends() {
    let mut src = Vec::new();
    for i in 0..200 {
        src.extend_from_slice(format!("v {i}.125 {i}.5 1{}", if i % 2 == 0 { "\r\n" } else { "\r" }).as_bytes());
    }
    for portion in 1..80 {
        let ends = formula::portions(&src, portion);
        assert_eq!(*ends.last().unwrap(), src.len());
        assert!(ends.windows(2).all(|w| w[0] < w[1]), "portion {portion}");
        let opt = Options { verify: false, block: 64, portion, ..Default::default() };
        let f = formula::encode_with(&src, &opt).unwrap();
        assert_eq!(eval(&f, 1).unwrap(), src, "portion {portion}");
    }
    // portions of a single huge number, and of nothing but line ends
    for src in [vec![b'7'; 5000], b"\n".repeat(3000), b"\r\n".repeat(1500)] {
        for portion in [1, 2, 3, 100, 1000] {
            let opt = Options { verify: false, block: 64, portion, ..Default::default() };
            if let Some(f) = formula::encode_with(&src, &opt) {
                assert_eq!(eval(&f, 2).unwrap(), src, "portion {portion}");
            }
        }
    }
}

/// Damaged streams (and the index): the decoder fails or gives other bytes, within time, never
/// panics or reserves what the data cannot hold.
#[test]
fn damaged_streams_do_not_hang() {
    use aqr_testkit::damaged;
    use std::time::Duration;
    let mut src = String::new();
    for i in 0..120 {
        src += &format!(
            "v {i}.5 {} 1e400
vt 0.{i} 0.5
f {i} {}/1 -{i}
",
            i * 3,
            i + 1
        );
    }
    let opt = Options { verify: false, block: 1 << 10, ..Default::default() };
    let f = formula::encode_with(src.as_bytes(), &opt).unwrap();
    let datas: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    let run = |datas: &[Vec<u8>]| {
        let inputs: Vec<&[u8]> = datas.iter().map(|d| d.as_slice()).collect();
        formula::decode(&[], &inputs, src.len() as u64, 1)
    };
    assert_eq!(run(&datas).unwrap(), src.as_bytes());
    for k in 0..datas.len() {
        if datas[k].is_empty() {
            continue;
        }
        damaged(&datas[k], 40, 24, Duration::from_secs(20), |v| {
            let mut d = datas.clone();
            d[k] = v.to_vec();
            let _ = run(&d);
        });
    }
}
