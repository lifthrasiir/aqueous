//! Edge cases of lines of text: numbers at the extremes of i64 with scales changing, the marks
//! of templates in the input, the limits of tokens, digit runs and columns, a locale of decimal
//! commas, and damaged streams.

use std::time::Duration;

use aqr_formula::{Formula, Node};
use aqr_log::formula::encode_in;
use aqr_log::module::{self, MAX_COLUMNS, MAX_DIGITS};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

fn eval(f: &Formula, len: usize) -> aqr_log::Result<Vec<u8>> {
    let Node::Apply(a) = &f.root else { panic!("not one application") };
    let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    module::decode(&a.params, &inputs, len as u64, 1)
}

/// The formula of `d`, which makes `d` again (and no other length).
fn roundtrip(d: &[u8]) -> Formula {
    let f = encode_in(d, usize::MAX, false, 1).unwrap();
    let back = eval(&f, d.len()).unwrap();
    assert!(back == d, "{}", String::from_utf8_lossy(&d[..d.len().min(300)]));
    assert!(eval(&f, d.len() + 1).is_err());
    f
}

fn params(f: &Formula) -> module::Params {
    let Node::Apply(a) = &f.root else { panic!() };
    module::Params::from_bytes(&a.params).unwrap()
}

/// Lines of `prefix` and each of `tokens`.
fn lines(prefix: &str, tokens: &[&str]) -> Vec<u8> {
    let mut s = String::new();
    for t in tokens {
        s += &format!("{prefix}{t}\n");
    }
    s.into_bytes()
}

#[test]
fn extremes_with_scale_changes() {
    // numbers of 18 digits at the largest and smallest values, in columns whose scale changes
    // from line to line, so that each is predicted from the one before rescaled
    let t = [
        "999999999999999999",
        "-999999999999999999",
        "0.000000000000000001",
        "-0.000000000000000001",
        "999999999999999.999",
        "-999999999999999.999",
        "0",
        "1.5",
        "999999999999999999",
        "0.999999999999999999",
        "-0.999999999999999999",
        "99999999999.9999999",
        "1e5",
        "123456789012345678e-17",
        "-9223372036854775808",
        "9223372036854775807",
        "9223372036854775808",
        "0.0000000000000000000000000001",
        "100000000000000000000000000000",
        "-0",
        "-0.0",
    ];
    let mut all = Vec::new();
    for i in 0..t.len() {
        all.extend(lines("v ", &t[i..]));
        let mut r: Vec<&str> = t.to_vec();
        r.rotate_left(i);
        all.extend(lines("w ", &r));
        // (the scale alternating)
        all.extend(lines("u ", &[t[i], t[(i * 7 + 3) % t.len()], t[i], t[(i * 7 + 3) % t.len()]]));
    }
    let f = roundtrip(&all);
    // (taken as numbers, not kept as they are)
    assert!(f.streams[1].data.is_empty() && !f.streams[5].data.is_empty());
    for tok in t {
        roundtrip(format!("a {tok}\nb {tok}\na {tok}\n").as_bytes());
        roundtrip(format!("{tok} {tok} {tok}\n{tok} 1 {tok}\n").as_bytes());
    }
}

#[test]
fn marks_in_the_input() {
    // lines holding the template marks (or NUL) are kept as they are, the others around them not
    for m in [0u8, 1, 2, 3, 4, 5] {
        let mut d = Vec::new();
        for i in 0..50 {
            d.extend(format!("t=12:00:{:02} id {i}\n", i % 60).bytes());
            if i % 7 == 3 {
                d.extend([b'x', m, b'y', b' ', b'1', b'\n']);
                d.extend([m, b'\n']);
                d.extend([b'7', b' ', b'1', m, b'\n']);
            }
        }
        let f = roundtrip(&d);
        if m != 0 {
            assert!(!f.streams[1].data.is_empty(), "mark {m}: kept lines");
        }
        // as the last line, with and without its LF
        let mut e = d.clone();
        e.extend([b'a', m, b'b']);
        roundtrip(&e);
        e.push(b'\n');
        roundtrip(&e);
    }
    // an address-like text and a time-like text next to a mark
    roundtrip(b"\x05 1.2.3.4\n\x04 12:00:00\n1.2.3.4 \x05\n12:00:00\x04\n10.0.0.1 x\n");
}

#[test]
fn token_length_limit() {
    // a number of 40 characters is a whole number, one of 41 a word
    for len in [38usize, 39, 40, 41, 42] {
        let zeros = "0".repeat(len - 2);
        let frac = format!("1.{}", "0".repeat(len - 2));
        let t = [format!("{zeros}.5"), frac, "5".repeat(len)];
        let mut d = String::new();
        for i in 0..60 {
            d += &format!("p {} {} {} {i}\n", t[0], t[1], t[2]);
        }
        roundtrip(d.as_bytes());
    }
}

#[test]
fn digit_run_limit() {
    // runs of 18 digits are a slot of a template, longer ones keep the token as a word
    for len in [1usize, 17, MAX_DIGITS - 1, MAX_DIGITS, MAX_DIGITS + 1, MAX_DIGITS + 2, 40] {
        let mut d = String::new();
        for i in 0..80u64 {
            let run = format!("{:0>len$}", i * 7919);
            let run = &run[run.len() - len..];
            d += &format!("id=a{run}z n{run}\nid=b{run}z.{run}\n");
        }
        roundtrip(d.as_bytes());
        roundtrip(d.replace("id=a", "id=a0").as_bytes());
    }
    roundtrip(
        &b"k=999999999999999999z\nk=9999999999999999999z\nk=000000000000000000z\nk=0000000000000000000z\n".repeat(30),
    );
}

#[test]
fn many_columns() {
    // a line of more fields than a segment may hold columns
    for n in [MAX_COLUMNS - 1, MAX_COLUMNS, MAX_COLUMNS + 1, MAX_COLUMNS + 100] {
        let mut d = String::new();
        for i in 0..n {
            d += &format!("{i} ");
        }
        d += "\nshort 1\nshort 2\n";
        let f = roundtrip(d.as_bytes());
        assert!(params(&f).columns <= MAX_COLUMNS);
    }
    // several lines each of fewer columns that together pass the bound
    let mut d = String::new();
    for l in 0..40 {
        for i in 0..(MAX_COLUMNS / 20) {
            d += &format!("{} ", i + l);
        }
        d += "\n";
    }
    let f = roundtrip(d.as_bytes());
    assert!(params(&f).columns <= MAX_COLUMNS);
}

#[test]
fn decimal_comma_locale() {
    let mut d = String::new();
    for i in 0..400 {
        d += &format!("t{} {:.3} 1.187,25 {},5 {};{}\n", i % 5, i as f64 * 0.37, i, i * 3, i);
    }
    roundtrip(d.as_bytes());
    let mut d = String::new();
    for i in 0..400 {
        d += &format!("{},{:03} {},{:03} a,b {}\n", i % 9 + 1, i % 1000, i, i * 7 % 1000, i);
    }
    roundtrip(d.as_bytes());
    // the sample ends past a quarter of a MiB: the tail has the other locale
    let mut d = String::new();
    while d.len() < (256 << 10) + 4000 {
        d += &format!("x {},{} {}\n", d.len() % 97, d.len() % 13 + 1, d.len() % 5);
    }
    for i in 0..300 {
        d += &format!("x {}.{} {}\n", i % 97, i % 13 + 1, i);
    }
    roundtrip(d.as_bytes());
}

#[test]
fn damaged_streams() {
    let mut d = String::new();
    for i in 0..60 {
        d += &format!("a {}.5 {} 10.0.0.{} 12:00:{:02} z{}\n", i * 3, i * 12345, i % 4, i % 60, i);
    }
    d += "999999999999999999 -999999999999999999 x\n";
    let f = roundtrip(d.as_bytes());
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
}
