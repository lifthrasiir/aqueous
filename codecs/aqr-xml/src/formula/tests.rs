use super::*;

/// The bytes of the slices below node `n` of `f`.
fn bytes(f: &Formula, n: &Node) -> Vec<u8> {
    match n {
        &Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| bytes(f, n)).collect(),
        _ => panic!("not slices"),
    }
}

/// The applications of formula `f`, one for each segment.
fn segments(f: &Formula) -> Vec<&aqr_formula::Apply> {
    let nodes = match &f.root {
        Node::Concat(ns) => ns.iter().collect(),
        n => vec![n],
    };
    nodes
        .into_iter()
        .map(|n| match n {
            Node::Apply(a) => &**a,
            _ => panic!("not an application"),
        })
        .collect()
}

/// The text of formula `f` of `len` bytes, or what is wrong with it.
fn eval(f: &Formula, len: usize) -> crate::Result<Vec<u8>> {
    let mut out = Vec::new();
    for a in segments(f) {
        let inputs: Vec<Vec<u8>> = a.inputs.iter().map(|n| bytes(f, n)).collect();
        let inputs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
        out.extend(decode(&a.params, &inputs, a.len, 1)?);
    }
    if out.len() == len { Ok(out) } else { Err(crate::corrupt()) }
}

const SMALL_LIMITS: Limits = Limits { segment: 1 << 30, containers: CONTAINERS, depth: DEPTH };

fn roundtrip_with(d: &[u8], lim: &Limits) -> (Formula, usize) {
    let (f, n) = encode_with(d, lim);
    assert_eq!(eval(&f, d.len()).unwrap(), d, "{}", String::from_utf8_lossy(d));
    assert!(eval(&f, d.len() + 1).is_err());
    (f, n)
}

fn roundtrip(d: &[u8]) -> (Formula, usize) {
    roundtrip_with(d, &SMALL_LIMITS)
}

/// The skeleton of the first segment of `f`, as text (the marks as `{v}`, `{o}`, `{c}`, `{e}`, `{x}`).
fn skeleton(f: &Formula) -> String {
    let a = segments(f)[0];
    bytes(f, &a.inputs[0])
        .iter()
        .map(|&b| match b {
            VALUE => "{v}".to_string(),
            OPEN => "{o}".to_string(),
            CLOSE => "{c}".to_string(),
            END => "{e}".to_string(),
            ESC => "{x}".to_string(),
            b => (b as char).to_string(),
        })
        .collect()
}

/// The values of the first segment of `f`, container by container.
fn containers(f: &Formula) -> Vec<Vec<u8>> {
    let a = segments(f)[0];
    let table = bytes(f, &a.inputs[1]);
    let values = bytes(f, &a.inputs[3]);
    let (mut at, mut from, mut out) = (0, 0, Vec::new());
    while at < table.len() {
        at += 1;
        let n = varint::get(&table, &mut at).unwrap() as usize;
        out.push(values[from..from + n].to_vec());
        from += n;
    }
    out
}

#[test]
fn structure() {
    let d = b"<?xml version=\"1.0\"?>\n<r a='1' b = \"2\">\n  <i id=\"7\">x &amp; y</i>\n  <e/>\n</r>\n";
    let (f, n) = roundtrip(d);
    assert_eq!(n, 4);
    assert_eq!(
        skeleton(&f),
        "<?xml version=\"1.0\"?>\n{o}<r a='{v}' b = \"{v}\">\n  {o}<i id=\"{v}\">{v}{c}\n  {o}<e/>\n{c}\n"
    );
    // (r@a, r@b, i@id, i/r text: in the order of the first values)
    assert_eq!(containers(&f), [&b"1\n"[..], b"2\n", b"7\n", b"x &amp; y\n"]);
}

#[test]
fn edge_cases() {
    for s in [
        &b""[..],
        b"x",
        b"<",
        b"<a",
        b"<a>",
        b"</a>",
        b"<a></a>",
        b"<a/>",
        b"<a b='1'/>",
        b"<a>text</a>",
        b"<a>text<!-- <b>tag</b> --> more</a>",
        b"<a><![CDATA[<b>not a tag</b>]]>x</a>",
        b"<a><?pi data <b>?>x</a>",
        b"<!DOCTYPE a [<!ENTITY e \"v>\"> <!ELEMENT a (#PCDATA)>]><a>t</a>",
        b"<!DOCTYPE a><a>t</a>",
        b"<a b = '1'  c=\"2\"\n\td='3'>t</a>",
        b"<a b='x\"y' c=\"x'y\">t</a>",
        b"<a b=''>t</a>",
        b"<a><b>1</c></a>",
        b"<a><b>1</a ></b>",
        b"<a><b>1</b ></a >",
        b"</a><b>x</b>",
        b"before<a>x</a>after\n",
        b"\xef\xbb\xbf<a>x</a>",
        b"<a>unterminated",
        b"<a b='unterminated>x</a>",
        b"<a b=\"x>y</a>",
        b"<a><!-- unterminated",
        b"<a><![CDATA[ unterminated",
        b"<a><? unterminated",
        b"<!DOCTYPE a [ unterminated",
        b"<a b>x</a><c d=e>y</c><f g= \"1\"h=\"2\">z</f>",
        b"<a>1 < 2 and 3 > 2</a>",
        b"<a>x<b>y</b>z<b>w</b></a>",
        b"<a>\n  <b>\n    <c>1</c>\n  </b>\n</a>",
        b"<a>line one\nline two\r\nline three</a><a>single</a>",
        b"<a b='1\n2'>x</a><a b='3'>y</a>",
        b"<a></a><a> </a><a>\n</a>",
        b"words and more words, not XML at all\n",
        b"\n\n  \n",
    ] {
        roundtrip(s);
    }
}

#[test]
fn special_bytes() {
    for s in [
        &b"\x01\x02\x03\x04\x05"[..],
        b"<a>\x01\x02</a>\x03\x04<!-- \x05 --><?pi \x01?>",
        b"<a b='\x01\x05'>\x04</a>",
        b"<a\x01b>x</a>",
        b"<a b\x02='1'>x</a>",
        b"<a>x</a\x03>",
        b"<a>x</a\x04>",
        b"<a b='1'\x01>x</a>",
        b"<a><b>\x05\x05</b></a>\x05",
        b"<a b=\x01'1'>x</a>",
        b"<![CDATA[\x01]]><a>1</a>",
    ] {
        roundtrip(s);
    }
    // (kept in the skeleton escaped)
    let (f, _) = roundtrip(b"<a>x</a>\x01<!--\x02-->");
    assert_eq!(skeleton(&f), "{o}<a>{v}{c}{x}{v}<!--{x}{o}-->");
}

#[test]
fn lines_in_values() {
    // (lengths apart once a value holds an LF, and for the ones before it)
    let d = b"<a><t>one</t><t>two\nlines</t><t>three</t><u>x</u><u>y</u></a>";
    let (f, _) = roundtrip(d);
    let a = segments(&f)[0];
    let table = bytes(&f, &a.inputs[1]);
    assert_eq!(&table[..], [1, 17, 0, 4]);
    assert_eq!(bytes(&f, &a.inputs[2]), [3, 9, 5]);
    let d = b"<a><t>cr\r\nlf</t><t>\n</t><t>x</t><t></t></a>";
    roundtrip(d);
    // (empty values of attributes)
    roundtrip(b"<a b=''/><a b=''/><a b='\n'/><a b=''/>");
}

#[test]
fn contexts() {
    let d = b"<r><a><x>1</x><y>2</y></a><b><x>3</x></b><x>4</x><a><x>5</x></a></r>";
    let (f, n) = roundtrip(d);
    // (a/x, a/y, b/x, r/x)
    assert_eq!(n, 4);
    assert_eq!(containers(&f), [&b"1\n5\n"[..], b"2\n", b"3\n", b"4\n"]);
}

#[test]
fn depth_and_container_bounds() {
    let deep = "<a>".repeat(300) + "x" + &"</a>".repeat(300);
    let lim = Limits { depth: 8, ..SMALL_LIMITS };
    let (f, _) = roundtrip_with(deep.as_bytes(), &lim);
    // (the first 8 elements are coded, the others kept)
    assert_eq!(skeleton(&f).matches("{o}").count(), 8);
    assert_eq!(segments(&f)[0].params[1], 8);
    roundtrip(deep.as_bytes());
    // elements past a bound among others
    let mixed = "<a><b><c><d>1</d>2<e/></c>3</b>4<f g='5'>6</f></a>".repeat(20);
    for depth in 0..6 {
        roundtrip_with(mixed.as_bytes(), &Limits { depth, ..SMALL_LIMITS });
    }
    // more contexts than a segment has containers: their values kept, as are tags of attributes
    let mut many = String::new();
    for i in 0..100 {
        many += &format!("<e{i} a{i}='{i}'>t{i}</e{i}><f x='{i}' y{i}='1'>u</f>");
    }
    for containers in [0, 1, 2, 3, 10, 50, 99] {
        let (f, n) = roundtrip_with(many.as_bytes(), &Limits { containers, ..SMALL_LIMITS });
        assert!(n <= containers);
        assert!(segments(&f)[0].params[0] as usize <= containers);
    }
}

#[test]
fn more_contexts_than_the_bound() {
    // (the bound of the encoder, past it the rest kept; the decoder's is higher)
    let mut s = String::from("<r>");
    for i in 0..CONTAINERS + 500 {
        s += &format!("<e{i} a='{i}'>t{i}</e{i}>");
    }
    s += "</r>";
    let (f, n) = roundtrip(s.as_bytes());
    assert_eq!(n, CONTAINERS);
    assert_eq!(segments(&f)[0].params[..3], [0x80, 0x20, 2]);
}

#[test]
fn segments_cut_after_end_tags() {
    let mut s = String::from("<?xml version='1.0'?>\n<root>\n");
    for i in 0..300 {
        s += &format!("<rec id='{i}'><name>n{}</name><v>{}</v></rec>\n", i % 7, i * 3);
        if i == 100 {
            s += "<!-- a comment --><rec id='x'><name>long</name></rec>stray text\n";
        }
    }
    s += "</root>\n";
    let d = s.as_bytes();
    let (one, _) = roundtrip(d);
    assert!(matches!(one.root, Node::Apply(_)));
    let (f, _) = roundtrip_with(d, &Limits { segment: 500, ..SMALL_LIMITS });
    let ns = segments(&f);
    assert!(ns.len() >= 8, "{}", ns.len());
    // (the segments after the first start in the root: its name in their params)
    assert!(ns[1..].iter().all(|a| a.params.windows(4).any(|w| w == b"root")));
    assert!(ns[0].params.len() < ns[1].params.len());
    for segment in [1, 2, 7, 100, 1000] {
        roundtrip_with(d, &Limits { segment, ..SMALL_LIMITS });
    }
    // (no element to cut after: one segment)
    let flat = "<a>x</a>".repeat(100) + "<b><c>1</c>";
    let (f, _) = roundtrip_with(flat.as_bytes(), &Limits { segment: 10, ..SMALL_LIMITS });
    assert_eq!(segments(&f).len(), 51);
    // (elements left open: cut where up to CUT_OPEN are, not more)
    let (f, _) = roundtrip_with(("<a><b>1</b>".repeat(100)).as_bytes(), &Limits { segment: 10, ..SMALL_LIMITS });
    assert_eq!(segments(&f).len(), CUT_OPEN + 1);
    // (and after an empty element: rows of a table)
    let rows = "<t>".to_string() + &"<row a='1' b=\"x\"/>\n".repeat(100) + "</t>";
    let (f, _) = roundtrip_with(rows.as_bytes(), &Limits { segment: 100, ..SMALL_LIMITS });
    assert!(segments(&f).len() > 15);
}

#[test]
fn small_containers_share_a_stream() {
    let mut s = String::from("<r>");
    for i in 0..6000 {
        s += &format!("<a>{}</a><b c='{}'>{}</b>", "x".repeat(i % 5 + 1), i, i % 3);
    }
    s += "<z>1</z></r>";
    let (f, n) = roundtrip(s.as_bytes());
    assert_eq!(n, 4);
    // (the skeleton, table, lens, a stream of each container of 4 KiB or more, the rest in one)
    let big = containers(&f).iter().filter(|c| c.len() >= SMALL).count();
    let small = containers(&f).iter().filter(|c| c.len() < SMALL).count();
    assert!(big >= 2 && small >= 1);
    assert_eq!(f.streams.len(), 3 + big + 1);
    assert!(f.streams[3..].iter().all(|s| s.content && [Kind::Text, Kind::Words, Kind::Column].contains(&s.kind)));
}

#[test]
fn not_xml() {
    assert!(encode(&b"words and more words\n".repeat(5000), 1).is_none());
    assert!(encode(b"<a>x</a>", 1).is_none());
    assert!(encode(&b"<a>x</a>".repeat(10000), 1).is_some());
    assert!(encode(&b"<!-- c -->\n".repeat(10000), 1).is_none());
    let pad = |s: &str| format!("{s}{}", " ".repeat(MIN_LEN));
    for yes in [
        "<?xml version='1.0'?><a/>",
        "\u{feff}  \n<a>",
        "<?xml version='1.0'?>\n<!-- c -->\n<!DOCTYPE a [<!ENTITY e 'x'>]>\n<a b='1'>",
        "<!DOCTYPE note SYSTEM \"n.dtd\"><note>",
        "<rss version=\"2.0\">",
        "<svg xmlns='http://www.w3.org/2000/svg'>",
    ] {
        assert!(starts(pad(yes).as_bytes()), "{yes}");
        assert!(!starts(yes.as_bytes()), "{yes}: short");
    }
    for no in [
        "<!DOCTYPE html><html>",
        "<!doctype HTML>\n<html lang='en'>",
        "<html><head>",
        "<HTML>",
        "<?xml version='1.0'?><!DOCTYPE html PUBLIC '-//W3C//DTD XHTML 1.0//EN'><html>",
        "<?xml version='1.0'?><html xmlns='http://www.w3.org/1999/xhtml'>",
        "hello <a>",
        "{\"a\":1}",
        "<?xml version='1.0'",
        "<",
        "<!-- unterminated",
        "<!x>",
    ] {
        assert!(!starts(pad(no).as_bytes()), "{no}");
    }
    // (random bytes round-trip, whatever they are)
    let mut rng = aqr_common::rng::Rng::new(3);
    let alphabet = b"<>/=\"' \n\r\t!?-[]abc\x01\x02\x03\x04\x05";
    for _ in 0..300 {
        let n = rng.below(400) as usize;
        let d: Vec<u8> = (0..n).map(|_| alphabet[rng.below(alphabet.len() as u64) as usize]).collect();
        roundtrip(&d);
        for segment in [3, 20] {
            roundtrip_with(&d, &Limits { segment, containers: 5, depth: 3 });
        }
    }
}

#[test]
fn corrupt_input() {
    let s = "<r a='1'><i id=\"7\">x &amp;\ny</i><e/><!-- c -->t</r>\n".repeat(10);
    let (f, _) = roundtrip(s.as_bytes());
    let a = segments(&f)[0];
    let mut rng = aqr_common::rng::Rng::new(5);
    for _ in 0..20000 {
        let mut streams: Vec<Vec<u8>> = a.inputs.iter().map(|n| bytes(&f, n)).collect();
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

#[test]
fn damaged_streams() {
    let s = "<r a='1'><i id=\"7\">x &amp;\ny</i><e/><!-- c -->t</r>\n<s b='2'>z</s>".repeat(6);
    let (f, _) = roundtrip(s.as_bytes());
    let a = segments(&f)[0];
    let streams: Vec<Vec<u8>> = a.inputs.iter().map(|n| bytes(&f, n)).collect();
    for (k, st) in streams.iter().enumerate() {
        if st.is_empty() {
            continue;
        }
        aqr_testkit::damaged(st, 40, 64, std::time::Duration::from_secs(20), |v| {
            let inputs: Vec<&[u8]> = streams.iter().enumerate().map(|(i, s)| if i == k { v } else { &s[..] }).collect();
            let _ = decode(&a.params, &inputs, s.len() as u64, 1);
        });
    }
    aqr_testkit::damaged(&a.params, 40, 64, std::time::Duration::from_secs(20), |v| {
        let inputs: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
        let _ = decode(v, &inputs, s.len() as u64, 1);
    });
}

#[test]
fn inconsistent_streams() {
    let d = b"<r a='1'><i>x</i><i>y\nz</i></r>";
    let (f, _) = roundtrip(d);
    let a = segments(&f)[0];
    let ins: Vec<Vec<u8>> = a.inputs.iter().map(|n| bytes(&f, n)).collect();
    let run = |p: &[u8], i: &[Vec<u8>], len: usize| {
        let i: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
        decode(p, &i, len as u64, 1)
    };
    assert_eq!(run(&a.params, &ins, d.len()).unwrap(), d);
    // (bytes left in a stream, or missing from it)
    for k in 0..4 {
        let mut more = ins.clone();
        more[k].push(b'x');
        assert!(run(&a.params, &more, d.len()).is_err(), "{k}");
        let mut less = ins.clone();
        if less[k].pop().is_some() {
            assert!(run(&a.params, &less, d.len()).is_err(), "{k}");
        }
    }
    // (params of more containers, a greater depth is fine, a smaller is not; names that do not end)
    let mut p = a.params.clone();
    p[0] += 1;
    assert!(run(&p, &ins, d.len()).is_err());
    let mut p = a.params.clone();
    p[1] -= 1;
    assert!(run(&p, &ins, d.len()).is_err());
    assert!(run(&[], &ins, d.len()).is_err());
    assert!(run(&[0, 0, 1, 5, b'a'], &ins, d.len()).is_err());
    assert!(run(&[0, 0, 0, 0], &ins, d.len()).is_err());
    assert!(run(&a.params, &ins[..3], d.len()).is_err());
    // (VALUE with nothing open, END, CLOSE of nothing)
    let nothing: Vec<Vec<u8>> = vec![vec![VALUE], vec![], vec![], vec![]];
    for s in [vec![VALUE], vec![CLOSE], vec![END, b'<', b'/', b'a', b'>'], vec![ESC, b'a'], vec![OPEN, b'x']] {
        let mut i = nothing.clone();
        i[0] = s;
        assert!(run(&[0, 0, 0], &i, 1).is_err());
    }
}

#[test]
fn prose_and_columns_are_told() {
    let mut s = String::from("<r>");
    for i in 0..4000 {
        s += &format!(
            "<e id=\"{i}\"><p>the quick brown fox number {i} jumps over the lazy dog and runs far away</p><n>{}</n><h>{}</h><s>x{i}</s></e>",
            i * 7919 % 10007,
            "0123456789abcdef".repeat(4)
        );
    }
    s += "</r>";
    let (f, n) = roundtrip(s.as_bytes());
    assert_eq!(n, 5);
    let a = segments(&f)[0];
    let table = bytes(&f, &a.inputs[1]);
    // (the streams after skeleton, table, lens: in the order the containers are made, each of
    // 16 KiB or more apart: id, p, n, h, s)
    let kinds: Vec<Kind> = f.streams[3..].iter().map(|s| s.kind).collect();
    let sizes = containers(&f).iter().map(|c| c.len()).collect::<Vec<_>>();
    assert!(table.len() >= 10 && sizes.iter().all(|&n| n >= SMALL), "{sizes:?}");
    // prose: long values with spaces; the rest, though long in all, columns of short values (a
    // hexadecimal string has no spaces)
    assert_eq!(kinds, [Kind::Column, Kind::Words, Kind::Column, Kind::Column, Kind::Column]);
}
