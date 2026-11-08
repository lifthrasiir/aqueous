//! aqr-xsv as a module of entry formulas (see aqr-formula): the table rebuilt from its fields,
//! stored column by column (FORMAT.md, "The module"), text as it is and numbers as integers,
//! for the archive to compress.

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};
use aqr_num::style::{Style, choose};

use crate::parse::{self, Field, Record, Table};
use crate::table::{Dialect, K_NEW, K_NUM, K_TEXT, MAX_COLUMNS, P_DELTA, P_RAW};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("xsv-recons", 1).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::plan(u.params, u.inputs, u.output())?)
}

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

impl Streams {
    /// The lengths of the streams, in the order of [`STREAM_NAMES`].
    fn lens(&self) -> [usize; 7] {
        [&self.layout, &self.raw, &self.columns, &self.types, &self.text, &self.nums, &self.desc].map(|v| v.len())
    }
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

/// The bytes of a table that make a segment (an application of the module): records are
/// taken while one starts before this many bytes from the segment's start. A table of fewer
/// bytes is one application. The module holds its output, its inputs and the table's columns,
/// so a long table's would take several times its length at once.
pub const SEGMENT: usize = 64 << 20;

/// The formula of table `input`, or `None` where it is shorter than [`MIN_LEN`], does not
/// start as a table ([`parse::dialect`]), or a tenth of it or more is not of the table. It is
/// the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    encode_in(input, SEGMENT, true)
}

/// The formula of `input` as a table of dialect `dia`, whatever it holds.
pub fn encode_as(input: &[u8], dia: &Dialect) -> Formula {
    formula(input, dia, SEGMENT, false).0
}

/// [`encode`] in segments of `segment` bytes (the test of segments at lengths shorter than
/// [`SEGMENT`]); `check` that no tenth of the table is not of it.
pub fn encode_in(input: &[u8], segment: usize, check: bool) -> Option<Formula> {
    if input.len() < MIN_LEN {
        return None;
    }
    let dia = dialect_of(input)?;
    let (f, raw) = formula(input, &dia, segment, check);
    (!check || raw * 10 < input.len()).then_some(f)
}

/// The formula of `input` as a table of dialect `dia`, and the bytes of its records kept as
/// they are (unless `!check`, when the count may stop early).
///
/// The file is cut into segments at records' starts ([`SEGMENT`]); the columns' count is the
/// whole file's, so that each segment is an application of the same params. The table of a
/// segment is parsed twice for a file in several (the count first, then the streams), so that
/// not more than a segment's table is held. A file in one segment is one application of the
/// streams; in more, an application of slices of the same seven streams, the segments' parts
/// following one another (so that the archive compresses them as it would the file's whole),
/// and a Concat of them. Each segment starts its columns afresh (the previous number's
/// descriptor and value), as each application does.
fn formula(input: &[u8], dia: &Dialect, segment: usize, check: bool) -> (Formula, usize) {
    let segment = segment.max(1);
    let width = |r: &Record| match r {
        Record::Fields { fields, .. } => fields.len(),
        Record::Raw(_) => 0,
    };
    // the first pass: the segments, the most fields of a record and the bytes kept as they are
    let (mut starts, mut widest, mut raw) = (vec![0usize], 0, 0);
    let mut first: Option<Table> = loop {
        let from = *starts.last().unwrap();
        let t = parse::parse_part(input, from, from.saturating_add(segment), dia, usize::MAX);
        widest = widest.max(t.records.iter().map(width).max().unwrap_or(0));
        raw += t.raw;
        let end = t.end;
        if end >= input.len() {
            // (a file in one segment keeps its table)
            break (starts.len() == 1).then_some(t);
        }
        starts.push(end);
        if check && raw * 10 >= input.len() {
            // (too much is not of the table: the formula is not used)
            return (Formula::default(), raw);
        }
    };
    let ncols = widest.clamp(1, MAX_COLUMNS);
    let mut s = Streams::default();
    let mut parts: Vec<([u64; 7], u64)> = Vec::new();
    for (k, &from) in starts.iter().enumerate() {
        let t = match k {
            0 if first.is_some() => first.take().unwrap(),
            _ => parse::parse_part(input, from, from.saturating_add(segment), dia, usize::MAX),
        };
        let before = s.lens();
        build(input, dia, &t, ncols, &mut s);
        let now = s.lens();
        parts.push((std::array::from_fn(|j| (now[j] - before[j]) as u64), (t.end - from) as u64));
    }
    let mut f = Formula::default();
    let params = dia.to_params(ncols);
    let kinds = [Kind::Unknown, Kind::Text, Kind::Unknown, Kind::Unknown, Kind::Text, Kind::Unknown, Kind::Unknown];
    let data = [s.layout, s.raw, s.columns, s.types, s.text, s.nums, s.desc];
    let whole: Vec<Node> = (STREAM_NAMES.iter().zip(kinds).zip(data))
        .map(|((name, kind), d)| f.push_as(format!("xsv/{name}"), kind, d))
        .collect();
    let base = f.streams.len() - 7;
    if let [(_, len)] = &parts[..] {
        f.root = Node::apply(MODULE, params, whole, *len);
        return (f, raw);
    }
    let mut at = [0u64; 7];
    let nodes: Vec<Node> = (parts.into_iter())
        .map(|(lens, len)| {
            let inputs: Vec<Node> = (0..7).map(|j| Node::slice(base + j, at[j], lens[j])).collect();
            at.iter_mut().zip(lens).for_each(|(a, n)| *a += n);
            Node::apply(MODULE, params.clone(), inputs, len)
        })
        .collect();
    f.root = Node::concat(nodes);
    (f, raw)
}

/// Appends the segment of records `t` of `input`, its columns of the file's `ncols`, to `s`.
fn build(input: &[u8], dia: &Dialect, t: &Table, ncols: usize, s: &mut Streams) {
    // the fields of each column, in order
    let mut cols: Vec<Vec<usize>> = vec![Vec::new(); ncols];
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
        column(input, t, ids, dia.quote, s);
    }
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

    /// The file of a formula of segments (an application, or a Concat of them).
    fn eval_segments(f: &Formula) -> Vec<u8> {
        let apply = |n: &Node| {
            let Node::Apply(a) = n else { panic!("not an application") };
            let inputs: Vec<&[u8]> = (a.inputs.iter())
                .map(|n| match n {
                    &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
                    _ => panic!("not a slice"),
                })
                .collect();
            decode(&a.params, &inputs, a.len, 1).unwrap()
        };
        match &f.root {
            Node::Concat(ns) => ns.iter().flat_map(apply).collect(),
            n => apply(n),
        }
    }

    /// A table of numbers that change slowly, text, quoted fields holding line ends, an
    /// irregular record and a wide one.
    fn table(rows: usize) -> Vec<u8> {
        let mut rng = aqr_common::rng::Rng::new(7);
        let mut s = String::from("id,t,x,name,note\n");
        for i in 0..rows {
            let x = 1000 + i as i64 * 3 + rng.below(5) as i64;
            match i % 97 {
                5 | 17 | 30 | 60 => s += &format!("{i},{},{x}.50,\"multi\nline, \"\"q\"\" {i}\",\"a\r\nb\"\n", i * 7),
                90 => s += "\"bad\"x,1\n",
                91 => s += &format!("{i},1,2,3,4,5,6\n"),
                _ => s += &format!("{i},{},{x}.25,name{},n\n", i * 7, rng.below(50)),
            }
        }
        s.into_bytes()
    }

    #[test]
    fn segments() {
        let d = table(3000);
        let whole = encode_in(&d, usize::MAX, true).unwrap();
        assert!(matches!(whole.root, Node::Apply(_)));
        assert_eq!(eval_segments(&whole), d);
        assert_eq!(whole.streams.len(), 7);
        for segment in [1, 100, 1000, 4096, 50_000, d.len() / 2] {
            let f = encode_in(&d, segment, true).unwrap();
            assert_eq!(eval_segments(&f), d, "segment {segment}");
            assert_eq!(f.streams.len(), 7);
            let Node::Concat(ns) = &f.root else { panic!("not segments at {segment}") };
            let total: u64 = ns
                .iter()
                .map(|n| match n {
                    Node::Apply(a) => a.len,
                    _ => panic!(),
                })
                .sum();
            assert_eq!(total, d.len() as u64);
            // (the segments end at records' starts: not within a quoted field's line end)
            let mut at = 0;
            for n in ns {
                let Node::Apply(a) = n else { panic!() };
                at += a.len as usize;
                assert!(at == d.len() || d[at - 1] == b'\n');
                let t = parse::parse_part(&d, at.saturating_sub(a.len as usize), at, &csv(), usize::MAX);
                assert_eq!(t.end, at);
            }
        }
        // the cut goes after the quoted line end, never within a record
        let d = b"a,b\n\"x\ny\",2\n\"p\nq\",3\n".repeat(1000);
        let f = encode_in(&d, 7, false).unwrap();
        assert_eq!(eval_segments(&f), d);
        let f = encode_as(&d, &csv());
        assert!(matches!(f.root, Node::Apply(_)));
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
            let mut streams: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
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
