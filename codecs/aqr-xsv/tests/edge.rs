//! Edge cases of delimited tables: numbers whose differences pass i64, a byte order mark before
//! a quoted field, delimiters and terminators of four bytes (partly overlapping the text), tables
//! of the most columns, and damaged streams.

use std::time::Duration;

use aqr_formula::{Formula, Node};
use aqr_xsv::formula::{encode, encode_as};
use aqr_xsv::module;
use aqr_xsv::table::{Dialect, MAX_COLUMNS, MAX_SEP};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

fn eval(f: &Formula, len: usize) -> aqr_xsv::Result<Vec<u8>> {
    let Node::Apply(a) = &f.root else { panic!("not one application") };
    let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    module::decode(&a.params, &inputs, len as u64, 1)
}

fn roundtrip(d: &[u8], dia: &Dialect) -> Formula {
    let f = encode_as(d, dia);
    let back = eval(&f, d.len()).unwrap();
    assert!(back == d, "{}", String::from_utf8_lossy(&d[..d.len().min(300)]));
    assert!(eval(&f, d.len() + 1).is_err());
    f
}

fn csv() -> Dialect {
    Dialect { delim: b",".to_vec(), quote: Some(b'"'), term: vec![] }
}

#[test]
fn differences_at_i64_extremes() {
    // columns of numbers whose consecutive differences wrap (the delta predictor wraps), at the
    // largest numbers the style codes and at the rest of 18 digits
    let vals = [
        "999999999999999999",
        "-999999999999999999",
        "999999999999999999",
        "0",
        "-999999999999999999",
        "-9223372036854775808",
        "9223372036854775807",
        "-9223372036854775808",
        "9223372036854775807",
        "9223372036854775808",
        "0.000000000000000001",
        "-0.000000000000000001",
        "1e5",
        "123456789012345678e-17",
    ];
    let mut d = String::from("a,b,c\n");
    for i in 0..vals.len() * 4 {
        d += &format!("{},{},{}\n", vals[i % vals.len()], vals[(i * 5 + 1) % vals.len()], vals[(i / 2) % vals.len()]);
    }
    let f = roundtrip(d.as_bytes(), &csv());
    assert!(!f.streams[5].data.is_empty());
    // the same in a column that is ascending then falling by the whole range
    let mut d = String::new();
    for i in 0..300i64 {
        let v = if i % 2 == 0 { 999_999_999_999_999_999 - i } else { -999_999_999_999_999_999 + i };
        d += &format!("{v},{}\n", i);
    }
    roundtrip(d.as_bytes(), &csv());
    let f = encode(d.as_bytes(), 1);
    if let Some(f) = f {
        assert_eq!(eval(&f, d.len()).unwrap(), d.as_bytes());
    }
}

#[test]
fn bom_before_a_quoted_field() {
    let bom = "\u{feff}";
    for t in [
        format!("{bom}\"a\",b\n\"1\",2\n"),
        format!("{bom}\"a\",\"b\"\r\n\"1\",\"2\"\r\n"),
        format!("{bom}\"a\"\n\"1\"\n"),
        format!("{bom}\"unclosed,b\n1,2\n"),
        format!("\"a\",b\n{bom}\"1\",2\n"),
        format!("{bom}{bom}\"a\",b\n1,2\n"),
        format!("{bom}a,b\n\"1\",2\n"),
        format!("{bom}\"a\"\"x\",b\n\"1\",2\n"),
    ] {
        roundtrip(t.as_bytes(), &csv());
        roundtrip(t.as_bytes(), &Dialect { delim: b";".to_vec(), quote: Some(b'"'), term: vec![] });
    }
    // as a table taken whole: header with a BOM and quoted, rows not
    let mut d = format!("{bom}\"id\",\"x\",\"y\"\n");
    for i in 0..300 {
        d += &format!("{i},{}.5,\"t{i}\"\n", i * 3);
    }
    if let Some(f) = encode(d.as_bytes(), 1) {
        assert_eq!(eval(&f, d.len()).unwrap(), d.as_bytes());
    }
    roundtrip(d.as_bytes(), &csv());
}

#[test]
fn separators_of_four_bytes() {
    assert_eq!(MAX_SEP, 4);
    for (delim, term) in [
        (&b"<|>>"[..], &b"\r\n\r\n"[..]),
        (b"abab", b"ab\n\n"),
        (b"aaab", b"aaaa"),
        (b"\xe2\x90\x9f\x01", b"\xe2\x90\x9e\n"),
        (b"::::", b""),
        (b"\t\t\t\t", b"\n\n\n\n"),
        (b"ab", b"abab"),
        (b"a", b"aaaa"),
    ] {
        let dia = Dialect { delim: delim.to_vec(), quote: Some(b'"'), term: term.to_vec() };
        let nq = Dialect { quote: None, ..dia.clone() };
        let (dl, tl) = (String::from_utf8_lossy(delim).into_owned(), String::from_utf8_lossy(term).into_owned());
        let rec = |a: &str, b: &str| format!("{a}{dl}{b}{tl}");
        let mut t = String::new();
        for i in 0..40 {
            t += &rec(&i.to_string(), &format!("{}.25", i * 3));
        }
        // fields holding a part of the separators, and the separators in quotes
        t += &rec("aaa", "aab");
        t += &rec("ab", "aba");
        t += &rec("\"q", &format!("x{dl}y\""));
        t += &rec(&format!("\"x{tl}y\""), "1");
        t += &format!("tail{}", &dl[..dl.len().min(3)]);
        for dia in [&dia, &nq] {
            roundtrip(t.as_bytes(), dia);
            // (the last record ended by a terminator or without one)
            roundtrip(&t.as_bytes()[..t.len() - 3], dia);
            roundtrip(format!("{t}{tl}").as_bytes(), dia);
            roundtrip(&t.as_bytes()[..t.len() / 2], dia);
        }
    }
}

#[test]
fn wide_tables() {
    for n in [MAX_COLUMNS - 1, MAX_COLUMNS, MAX_COLUMNS + 1, MAX_COLUMNS * 2 + 3] {
        let row = |k: usize| (0..n).map(|i| (i * k).to_string()).collect::<Vec<_>>().join(",");
        let d = format!("{}\n{}\n{}\n", row(1), row(2), row(3));
        let f = roundtrip(d.as_bytes(), &csv());
        let Node::Apply(a) = &f.root else { panic!() };
        let (_, cols) = Dialect::from_params(&a.params).unwrap();
        assert_eq!(cols, n.min(MAX_COLUMNS));
        // the memory plan allows what the decoder takes
        assert!(
            module::plan(&a.params, &f.streams.iter().map(|s| s.data.len() as u64).collect::<Vec<_>>(), d.len() as u64)
                .is_some()
        );
    }
    // rows of different widths around the limit
    let row = |n: usize| (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let d = format!("{}\n{}\n{}\n{}\n", row(5), row(MAX_COLUMNS + 5), row(MAX_COLUMNS), row(1));
    roundtrip(d.as_bytes(), &csv());
}

#[test]
fn damaged_streams() {
    let mut d = String::from("id,name,x,when\n");
    for i in 0..40 {
        d += &format!("{i},\"n{i}\"\"q\",{}.5,2023-10-12 00:{:02}\n", i * 3, i % 60);
    }
    d += "999999999999999999,\"\",-999999999999999999,\n";
    let f = roundtrip(d.as_bytes(), &csv());
    let Node::Apply(a) = &f.root else { panic!() };
    let params = a.params.clone();
    let streams: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    for (k, s) in streams.iter().enumerate() {
        if s.is_empty() {
            continue;
        }
        aqr_testkit::damaged(s, 40, 40, Duration::from_secs(20), |v| {
            let mut ins: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
            ins[k] = v;
            let _ = module::decode(&params, &ins, d.len() as u64, 1);
        });
    }
    // damaged params
    aqr_testkit::damaged(&params, 40, 40, Duration::from_secs(20), |p| {
        let ins: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
        let _ = module::decode(p, &ins, d.len() as u64, 1);
        let _ = module::plan(p, &streams.iter().map(|s| s.len() as u64).collect::<Vec<_>>(), d.len() as u64);
    });
}

#[test]
fn delimiter_prefix_is_no_line_end() {
    // (a byte that starts the delimiter, in the field after one, is not the end of the line)
    let dia = Dialect { delim: b"::::".to_vec(), quote: Some(b'"'), term: vec![] };
    for t in ["a:::::b", "a:::::b\n", "a::::::b::::c", "a:::b::::c\r\nd:::::e", ":::::", "a::::\":x\"::::::"] {
        roundtrip(t.as_bytes(), &dia);
    }
    // the USV delimiter's first byte is that of the euro sign
    let usv = Dialect { delim: "\u{241f}".as_bytes().to_vec(), quote: None, term: vec![] };
    roundtrip("\u{20ac}\u{241f}\u{20ac}\u{20ac}\n\u{241f}\u{20ac}\u{241f}\n".as_bytes(), &usv);
}
