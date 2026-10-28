//! aqr-xsv as a module of entry formulas (see aqr-formula): the table rebuilt from its fields,
//! stored column by column (FORMAT.md, "The module"), text as it is and numbers as integers,
//! for the archive to compress.

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};
use aqr_num::style::{Style, choose};

use crate::parse::{self, Field, Record, Table};
use crate::table::{Dialect, K_NEW, K_NUM, K_TEXT, MAX_COLUMNS, P_DELTA, P_RAW};

pub use crate::module::decode;

pub const MODULE: Module = Module { name: "xsv-recons", version: 1 };

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 7] = ["layout", "raw", "columns", "types", "text", "nums", "desc"];

/// A file shorter than this is not taken.
pub const MIN_LEN: usize = 16 << 10;

/// The bytes at the start of a file that tell its dialect.
const SAMPLE: usize = 64 << 10;

/// The tokens of a column that its numbers' style is chosen from.
const STYLE_SAMPLE: usize = 512;

/// The dialect of `d`, told from its start.
fn dialect_of(d: &[u8]) -> Option<Dialect> {
    let whole = d.len() <= SAMPLE;
    parse::dialect(&d[..d.len().min(SAMPLE)], whole)
}

/// Whether `d` is a file this crate takes: of at least [`MIN_LEN`] bytes, starting as a table
/// of a dialect ([`parse::dialect`]).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= MIN_LEN && dialect_of(d).is_some()
}

/// Whether token `t` may be a number: of a sign, a digit or a point first, and short.
fn numeric(t: &[u8]) -> bool {
    matches!(t.first(), Some(b'0'..=b'9' | b'-' | b'+' | b'.')) && t.len() <= 40
}

/// The bytes of field `f` of `d`: within the quotes, the doubled quotes made single (in `tmp`).
fn content<'a>(d: &'a [u8], f: &Field, q: Option<u8>, tmp: &'a mut Vec<u8>) -> &'a [u8] {
    let s = &d[f.start..f.end];
    match (f.escaped, q) {
        (true, Some(q)) => {
            tmp.clear();
            let mut i = 0;
            while i < s.len() {
                tmp.push(s[i]);
                i += if s[i] == q { 2 } else { 1 };
            }
            tmp
        }
        _ => s,
    }
}

/// The streams of a table: a column's are put one after the other.
#[derive(Default)]
struct Streams {
    layout: Vec<u8>,
    raw: Vec<u8>,
    columns: Vec<u8>,
    types: Vec<u8>,
    text: Vec<u8>,
    nums: Vec<u8>,
    desc: Vec<u8>,
}

/// The values of a column, as the stream `nums` holds them by predictor `pred`.
fn values(vals: &[i64], pred: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 2);
    let mut prev = 0i64;
    for &v in vals {
        varint::put_signed(&mut out, if pred == P_DELTA { v.wrapping_sub(prev) } else { v });
        prev = v;
    }
    out
}

/// The size of `b` compressed fast, which tells predictors apart.
fn packed(b: &[u8]) -> usize {
    zstd::bulk::compress(b, 1).map_or(b.len(), |c| c.len())
}

/// Puts the fields `ids` of column `c` of `d` (at their records' order) into the streams.
fn column(d: &[u8], t: &Table, ids: &[usize], q: Option<u8>, s: &mut Streams) {
    let (types0, text0, desc0) = (s.types.len(), s.text.len(), s.desc.len());
    let mut tmp = Vec::new();
    let mut toks: Vec<(usize, &[u8])> = Vec::new();
    for &i in ids {
        let f = &t.fields[i];
        if !f.escaped && numeric(&d[f.start..f.end]) {
            toks.push((0, &d[f.start..f.end]));
            if toks.len() == STYLE_SAMPLE {
                break;
            }
        }
    }
    let style: Style = choose(&toks);
    let mut vals = Vec::new();
    let mut last: Option<(u8, u8, i32)> = None;
    let mut scratch = Vec::with_capacity(64);
    let mut back = Vec::with_capacity(64);
    for &i in ids {
        let f = &t.fields[i];
        let c = content(d, f, q, &mut tmp);
        let num = match numeric(c) {
            true => style.encode(c, last.map_or(0, |l| l.2), &mut scratch).filter(|&(v, sc, mode, fl)| {
                back.clear();
                aqr_num::put(&mut back, v, sc, mode, fl);
                back == c
            }),
            false => None,
        };
        let kind = match num {
            Some((v, sc, mode, fl)) => {
                vals.push(v);
                if last == Some((mode, fl, sc)) {
                    K_NUM
                } else {
                    last = Some((mode, fl, sc));
                    s.desc.extend_from_slice(&[mode, fl]);
                    varint::put_signed(&mut s.desc, sc as i64);
                    K_NEW
                }
            }
            None => {
                s.text.extend_from_slice(c);
                s.text.push(0);
                K_TEXT
            }
        };
        s.types.push(kind << 1 | f.quoted as u8);
    }
    let raw = values(&vals, P_RAW);
    let delta = values(&vals, P_DELTA);
    let (pred, nums) = match packed(&delta) < packed(&raw) {
        true => (P_DELTA, delta),
        false => (P_RAW, raw),
    };
    s.columns.push(pred);
    for n in [s.types.len() - types0, s.text.len() - text0, nums.len(), s.desc.len() - desc0] {
        varint::put(&mut s.columns, n as u64);
    }
    s.nums.extend_from_slice(&nums);
}

/// The formula of table `input`, or `None` where it is shorter than [`MIN_LEN`], does not
/// start as a table ([`parse::dialect`]), or a tenth of it or more is not of the table. It is
/// the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN {
        return None;
    }
    let dia = dialect_of(input)?;
    let t = parse::parse(input, &dia, usize::MAX);
    (t.raw * 10 < input.len()).then(|| formula(input, &dia, &t))
}

/// The formula of `input` as a table of dialect `dia`, whatever it holds.
pub fn encode_as(input: &[u8], dia: &Dialect) -> Formula {
    formula(input, dia, &parse::parse(input, dia, usize::MAX))
}

/// The formula of `input`, cut into records `t` of dialect `dia`.
fn formula(input: &[u8], dia: &Dialect, t: &Table) -> Formula {
    let width = |r: &Record| match r {
        Record::Fields { fields, .. } => fields.len(),
        Record::Raw(_) => 0,
    };
    let ncols = t.records.iter().map(width).max().unwrap_or(0).clamp(1, MAX_COLUMNS);
    // the fields of each column, in order
    let mut cols: Vec<Vec<usize>> = vec![Vec::new(); ncols];
    let mut s = Streams::default();
    for r in &t.records {
        match r {
            Record::Raw(range) => {
                s.layout.push(0);
                varint::put(&mut s.layout, range.len() as u64);
                s.raw.extend_from_slice(&input[range.clone()]);
            }
            Record::Fields { fields, end } => {
                varint::put(&mut s.layout, (fields.len() as u64) << 2 | end);
                for (j, i) in fields.clone().enumerate() {
                    cols[j.min(ncols - 1)].push(i);
                }
            }
        }
    }
    for ids in &cols {
        column(input, t, ids, dia.quote, &mut s);
    }
    let mut f = Formula::default();
    let inputs = vec![
        f.push("xsv/layout", s.layout),
        f.push_as("xsv/raw", Kind::Text, s.raw),
        f.push("xsv/columns", s.columns),
        f.push("xsv/types", s.types),
        f.push_as("xsv/text", Kind::Text, s.text),
        f.push("xsv/nums", s.nums),
        f.push("xsv/desc", s.desc),
    ];
    f.root = Node::apply(MODULE, dia.to_params(ncols), inputs, input.len() as u64);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(f: &Formula, len: usize) -> crate::Result<Vec<u8>> {
        let Node::Apply(a) = &f.root else { panic!() };
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        decode(&a.params, &inputs, len as u64, 1)
    }

    fn roundtrip(d: &[u8], dia: &Dialect) -> Formula {
        let f = encode_as(d, dia);
        assert_eq!(eval(&f, d.len()).unwrap(), d, "{}", String::from_utf8_lossy(d));
        assert!(eval(&f, d.len() + 1).is_err());
        f
    }

    fn csv() -> Dialect {
        Dialect { delim: b",".to_vec(), quote: Some(b'"'), term: vec![] }
    }

    #[test]
    fn edge_cases() {
        let d = csv();
        for s in [
            &b"a,b\n1,2\n"[..],
            b"a,b\n1,2",
            b"a,b\r\n1,2\r\n",
            b"\n\n,\n,,,\n",
            b"x,\"q\"\"uote\",\"\"\n\"multi\nline\",\"1.5\"\n",
            b"\"bad\"x,1\n1,2\n",
            b"\"unclosed,1\n2,3\n",
            b"1,-0,0.10,1e5,1.5e+30,007,+3,.5,-.25,9223372036854775808,nan\n2,-0.0,0.20,,,,,,,,\n",
            b"a,b,c,d,e\n1\n1,2,3,4,5,6,7,8\n",
            b"tab\tsep,\x01ctrl,\xff\xfe\n",
            b"",
        ] {
            roundtrip(s, &d);
        }
        // fields holding a zero byte: the record kept as it is
        let f = roundtrip(b"a,b\x00c\n1,2\n", &d);
        assert_eq!(f.streams[1].data, b"a,b\x00c\n");
        // more fields than the most columns
        let wide: Vec<u8> = (0..MAX_COLUMNS + 10).map(|i| i.to_string()).collect::<Vec<_>>().join(",").into();
        roundtrip(&wide, &d);
    }

    #[test]
    fn dialects() {
        let tsv = Dialect { delim: b"\t".to_vec(), quote: None, term: vec![] };
        roundtrip(b"a\t\"b\t1\n\"x\t2\r\n", &tsv);
        let usv = Dialect { delim: "␟".as_bytes().to_vec(), quote: None, term: "␞\n".as_bytes().to_vec() };
        roundtrip("a␟b␞\n1␟2␞\nmulti\nline␟3␞\n4␟5".as_bytes(), &usv);
        roundtrip("a␟b␞1␟2␞".as_bytes(), &usv);
        let ascii = Dialect { delim: vec![0x1f], quote: None, term: vec![0x1e] };
        roundtrip(b"a\x1fb\x1e1\x1f2\x1e", &ascii);
    }

    #[test]
    fn numbers() {
        let mut s = String::from("id,x,y,z,name\n");
        for i in 0..2000 {
            let x = (i as f64 * 0.37).sin() * 1000.0;
            // (%e as C prints it: a sign and two digits in the exponent)
            let e = format!("{x:.5e}");
            let (m, ex) = e.split_once('e').unwrap();
            let ex: i32 = ex.parse().unwrap();
            s += &format!(
                "{},{:.6},{},{m}e{}{:02},n{}\n",
                i,
                x,
                x as f32,
                if ex < 0 { '-' } else { '+' },
                ex.abs(),
                i % 7
            );
        }
        let f = roundtrip(s.as_bytes(), &csv());
        // (the numbers coded: the text holds the header and the names only)
        assert!(f.streams[4].data.len() < 2000 * 4, "{}", f.streams[4].data.len());
        let f = encode(s.as_bytes(), 1).unwrap();
        assert_eq!(eval(&f, s.len()).unwrap(), s.as_bytes());
    }

    #[test]
    fn not_tables() {
        assert!(encode(&b"words and more words\n".repeat(2000), 1).is_none());
        assert!(encode(&b"a,b\n".repeat(100), 1).is_none());
    }

    #[test]
    fn corrupt_input() {
        let s = "a,b,c\n1,\"x\"\"y\",2.5\n".repeat(10);
        let f = roundtrip(s.as_bytes(), &csv());
        let Node::Apply(a) = &f.root else { panic!() };
        let mut rng = aqr_common::rng::Rng::new(5);
        for _ in 0..20000 {
            let mut streams: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.clone()).collect();
            let k = rng.below(streams.len() as u64) as usize;
            if streams[k].is_empty() {
                continue;
            }
            let i = rng.below(streams[k].len() as u64) as usize;
            streams[k][i] = rng.below(256) as u8;
            let inputs: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
            let _ = decode(&a.params, &inputs, s.len() as u64, 1);
        }
    }
}
