//! Quoted-printable bodies through the formula and the module `b2t-recons-quopri`: the output of
//! real encoders (Python's `quopri` and `email`, Perl's MIME::QuotedPrint, Ruby's `pack("M")`:
//! `tests/data`), a canonical encoder's with each of its settings, where bodies are found (the
//! headers, the boundaries, mbox's `From `), exceptions, what is not taken, and layouts that
//! are not right turned down.

use aqr_b2t::quopri::formula;
use aqr_common::rng::Rng;
use aqr_common::varint;
use aqr_formula::{Apply, Formula};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

fn taken(d: &[u8]) -> Formula {
    let f = formula::encode(d, 1).expect("a body taken");
    assert_eq!(eval(&f).unwrap(), d);
    f
}

fn stream(f: &Formula, class: &str) -> Vec<u8> {
    f.streams.iter().find(|s| s.class == class).unwrap().data.to_vec()
}

fn data_of(f: &Formula) -> Vec<u8> {
    stream(f, "b2t-quopri/data")
}

fn not_taken(d: &[u8]) {
    if let Some(f) = formula::encode(d, 1) {
        assert_eq!(eval(&f).unwrap(), d);
        panic!("taken: {} bytes of data", data_of(&f).len());
    }
}

/// The text of a message body: HTML with accents and CJK in it, long lines, a trailing space.
fn body(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let w: [&str; 9] = [
        "the ",
        "caf\u{e9} ",
        "<a href=\"http://x.org/?a=1&b=2\">",
        "\u{c548}\u{b155} ",
        "= ",
        "\u{201c}quoted\u{201d}\n",
        "mail\n\n",
        "\ttab ",
        "word word word word word word word word word word word word word ",
    ];
    let mut t = Vec::new();
    while t.len() < n {
        t.extend_from_slice(w[rng.below(9) as usize].as_bytes());
    }
    t
}

/// A canonical encoder's text of `data`: a token (a byte, or a line break of CRLF or LF) escaped
/// where it is not printable ASCII (a space or tab before a line's end), a soft break before a
/// token that does not fit; `start` as Ruby's (a token's first character decides).
struct Enc {
    crlf: bool,
    lower: bool,
    line: usize,
    soft_end: bool,
    start: bool,
}

impl Enc {
    fn new() -> Enc {
        Enc { crlf: true, lower: false, line: 76, soft_end: false, start: false }
    }

    fn encode(&self, data: &[u8]) -> Vec<u8> {
        let eol: &[u8] = if self.crlf { b"\r\n" } else { b"\n" };
        let digits = if self.lower { b"0123456789abcdef" } else { b"0123456789ABCDEF" };
        let mut out = Vec::new();
        let mut col = 0;
        let mut i = 0;
        while i < data.len() {
            if data[i..].starts_with(eol) {
                out.extend_from_slice(eol);
                col = 0;
                i += eol.len();
                continue;
            }
            let b = data[i];
            let end = i + 1 == data.len() || data[i + 1..].starts_with(eol);
            let ws = b == b' ' || b == b'\t';
            let esc = !((33..=126).contains(&b) && b != b'=' || ws) || (ws && end);
            let w = if esc { 3 } else { 1 };
            let limit = if end { self.line } else { self.line - 1 };
            if col + if self.start { 1 } else { w } > limit {
                out.push(b'=');
                out.extend_from_slice(eol);
                col = 0;
            }
            match esc {
                true => out.extend_from_slice(&[b'=', digits[(b >> 4) as usize], digits[(b & 15) as usize]]),
                false => out.push(b),
            }
            col += w;
            i += 1;
        }
        if self.soft_end {
            out.push(b'=');
            out.extend_from_slice(eol);
        }
        out
    }
}

/// `body` (with the line breaks as the encoder's) in the headers of a message.
fn message(enc: &Enc, data: &[u8], after: &[u8]) -> Vec<u8> {
    let nl: &[u8] = if enc.crlf { b"\r\n" } else { b"\n" };
    let mut d = Vec::new();
    for l in
        ["From: a@b.org", "Content-Type: text/html; charset=utf-8", "Content-Transfer-Encoding: quoted-printable", ""]
    {
        d.extend_from_slice(l.as_bytes());
        d.extend_from_slice(nl);
    }
    d.extend(enc.encode(data));
    d.extend_from_slice(after);
    d
}

fn layout_of(f: &Formula) -> Vec<u8> {
    stream(f, "b2t-quopri/layout")
}

/// The data of `body` with its line breaks made `nl`.
fn with_breaks(data: &[u8], crlf: bool) -> Vec<u8> {
    let s = String::from_utf8_lossy(data).replace('\n', if crlf { "\r\n" } else { "\n" });
    s.into_bytes()
}

#[test]
fn real_encoders_need_no_exceptions() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");
    for name in ["python-quopri", "python-quoprimime", "python-email", "perl", "ruby"] {
        let d = std::fs::read(format!("{dir}{name}.eml")).unwrap();
        let f = taken(&d);
        // (count 0 after the run's line length: the layout is the gap, n, flags, line, count)
        let l = layout_of(&f);
        let mut at = 0;
        for _ in 0..2 {
            varint::get(&l, &mut at).unwrap();
        }
        at += 1;
        varint::get(&l, &mut at).unwrap();
        let count = varint::get(&l, &mut at).unwrap();
        assert_eq!(count, 0, "{name}");
        assert!(data_of(&f).len() > 3000, "{name}");
    }
}

#[test]
fn a_canonical_encoder_with_each_setting() {
    for (i, (crlf, lower, line, soft_end, start)) in [
        (true, false, 76, false, false),
        (false, false, 76, false, false),
        (true, true, 76, false, false),
        (false, true, 78, true, false),
        (true, false, 72, false, true),
        (false, false, 74, true, true),
        (true, false, 998, false, false),
        (true, false, 8, false, false),
    ]
    .into_iter()
    .enumerate()
    {
        let enc = Enc { crlf, lower, line, soft_end, start };
        let data = with_breaks(&body(6000, 30 + i as u64), crlf);
        let d = message(&enc, &data, b"");
        let f = taken(&d);
        assert_eq!(data_of(&f), data, "{i}");
        // (no exceptions: the layout is the gap, the bytes, the flags, the line and 0)
        let l = layout_of(&f);
        assert_eq!(*l.last().unwrap(), 0, "{i} {l:?}");
    }
}

#[test]
fn where_a_body_ends() {
    let enc = Enc { crlf: false, ..Enc::new() };
    let data = body(3000, 40);
    let b = enc.encode(&data);
    let head =
        b"Content-Type: multipart/alternative; boundary=\"=_part\"\nContent-Transfer-Encoding: QUOTED-PRINTABLE \n\n";
    // the line break before a boundary is not of the body
    let mut d =
        b"Content-Type: multipart/alternative; boundary=\"=_part\"\n\n--=_part\nContent-type: text/html\n".to_vec();
    d.extend_from_slice(b"content-transfer-encoding : Quoted-Printable\n\n");
    d.extend(&b);
    d.extend_from_slice(b"\n--=_part\nContent-Type: text/plain\n\nplain\n--=_part--\n");
    let f = taken(&d);
    assert_eq!(data_of(&f), data);
    // a signature's `-- ` ends nothing
    let mut sig = data.clone();
    sig.extend_from_slice(b"\n-- \nsomeone\n");
    let mut d = head.to_vec();
    d.extend(enc.encode(&sig));
    d.extend_from_slice(b"\n--=_part--\n");
    assert_eq!(data_of(&taken(&d)), sig);
    // mbox: the next message's `From `
    let mut d =
        b"From a@b.org Mon Jan  1 00:00:00 2024\nSubject: x\nContent-Transfer-Encoding: quoted-printable\n\n".to_vec();
    d.extend(&b);
    d.extend_from_slice(b"\n\nFrom c@d.org Mon Jan  1 00:00:01 2024\nSubject: y\n\nbody\n");
    let f = taken(&d);
    assert_eq!(&data_of(&f)[data.len()..], b"\n");
    // the end of the file
    let mut d = head.to_vec();
    d.extend(&b);
    assert_eq!(data_of(&taken(&d)), data);
    // a boundary token, not quoted; and a header that is not one
    let mut d =
        b"Content-Type: multipart/mixed; boundary=abc\nContent-Transfer-Encoding: quoted-printable\n\n".to_vec();
    d.extend(&b);
    d.extend_from_slice(b"\n--abc\nlast\n");
    assert_eq!(data_of(&taken(&d)), data);
    let mut d = b"Content-Transfer-Encoding: base64\n\n".to_vec();
    d.extend(&b);
    not_taken(&d);
    let mut d = b"Content-Transfer-Encoding: quoted-printable-not\n\n".to_vec();
    d.extend(&b);
    not_taken(&d);
    // (no blank line: the header block does not end)
    let mut d = b"Content-Transfer-Encoding: quoted-printable\nThis is text, not a header\n\n".to_vec();
    d.extend(&b);
    not_taken(&d);
}

#[test]
fn two_bodies_and_the_text_between() {
    let enc = Enc { crlf: false, ..Enc::new() };
    let (a, b) = (body(2000, 41), body(2500, 42));
    let mut d =
        b"Content-Type: multipart/mixed; boundary=\"X\"\n\n--X\nContent-Transfer-Encoding: quoted-printable\n\n"
            .to_vec();
    d.extend(enc.encode(&a));
    d.extend_from_slice(b"\n--X\nContent-Transfer-Encoding: quoted-printable\n\n");
    d.extend(enc.encode(&b));
    d.extend_from_slice(b"\n--X--\nepilogue\n");
    let f = taken(&d);
    assert_eq!(data_of(&f), [a, b].concat());
}

#[test]
fn exceptions_are_taken_where_few() {
    let enc = Enc { crlf: false, ..Enc::new() };
    let data = body(8000, 43);
    let mut text = enc.encode(&data);
    // (a few letters written as escapes: the encoder escaped what it need not)
    let mut n = 0;
    let mut i = 200;
    while i < text.len() - 4 && n < 12 {
        if text[i].is_ascii_lowercase() && text[i - 1] != b'=' && text[i - 2] != b'=' && text[i + 1] != b'=' {
            let b = text[i];
            text.splice(i..=i, [b'=', b"0123456789ABCDEF"[(b >> 4) as usize], b"0123456789ABCDEF"[(b & 15) as usize]]);
            n += 1;
            i += 300;
        } else {
            i += 1;
        }
    }
    assert_eq!(n, 12);
    let mut d = b"Content-Transfer-Encoding: quoted-printable\n\n".to_vec();
    d.extend(&text);
    let f = taken(&d);
    assert_eq!(data_of(&f), data);
    // (more than 2%: not an encoder that is modeled)
    let mut many = Vec::new();
    for (k, &b) in text.iter().enumerate() {
        match b.is_ascii_lowercase() && k % 20 == 0 && !text[k.saturating_sub(2)..k].contains(&b'=') {
            true => many.extend_from_slice(&[
                b'=',
                b"0123456789ABCDEF"[(b >> 4) as usize],
                b"0123456789ABCDEF"[(b & 15) as usize],
            ]),
            false => many.push(b),
        }
    }
    let mut d = b"Content-Transfer-Encoding: quoted-printable\n\n".to_vec();
    d.extend(&many);
    not_taken(&d);
}

#[test]
fn ends_in_a_soft_break_and_escaped_line_breaks() {
    let enc = Enc { crlf: true, soft_end: true, ..Enc::new() };
    let data = with_breaks(&body(3000, 44), true);
    let d = message(&enc, &data, b"\r\n--b\r\n");
    let mut d2 = d.clone();
    d2.splice(0..0, b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n".iter().copied());
    assert_eq!(data_of(&taken(&d2)), data);
    // a CRLF written as `=0D=0A` (a binary encoder's): exceptions, the same bytes
    let mut text = Vec::new();
    for (k, l) in data.split(|&b| b == b'\n').enumerate() {
        if k > 0 {
            text.extend_from_slice(b"\r\n");
        }
        text.extend_from_slice(&Enc { crlf: true, ..Enc::new() }.encode(l.strip_suffix(b"\r").unwrap_or(l)));
    }
    let mut d = b"Content-Transfer-Encoding: quoted-printable\r\n\r\n".to_vec();
    d.extend(&text);
    if let Some(f) = formula::encode(&d, 1) {
        assert_eq!(eval(&f).unwrap(), d);
    }
}

#[test]
fn not_quoted_printable() {
    let head = b"Content-Transfer-Encoding: quoted-printable\n\n";
    let enc = Enc { crlf: false, ..Enc::new() };
    let good = enc.encode(&body(4000, 45));
    // (an equals sign alone, one with one digit, a byte of 8 bits, a lone CR, mixed case,
    // mixed breaks)
    let bads: [&[u8]; 6] = [b"x = y", b"=4", "\u{e9}".as_bytes(), b"\r", b"=3d=3D", b"a\r\nb"];
    for bad in bads {
        let mut d = head.to_vec();
        d.extend(&good);
        d.extend_from_slice(bad);
        not_taken(&d);
    }
    // (text with no escape and no soft break: nothing to take)
    let mut d = head.to_vec();
    d.extend(b"short words only here\n".repeat(100));
    not_taken(&d);
    // (shorter than the least)
    let mut d = head.to_vec();
    d.extend_from_slice(b"caf=C3=A9\n");
    not_taken(&d);
    // (a small share of a big file)
    let mut d = head.to_vec();
    d.extend(enc.encode(&body(600, 46)));
    d.extend_from_slice(b"\nFrom a:b\n");
    d.extend(body(200_000, 47));
    not_taken(&d);
}

/// A body of the layout: its gap, bytes, flags, line length and exceptions.
type Body<'a> = (u64, u64, u8, u64, &'a [(u64, u8)]);

fn layout(runs: &[Body]) -> Vec<u8> {
    let mut l = Vec::new();
    for &(gap, n, flags, line, exc) in runs {
        varint::put(&mut l, gap);
        varint::put(&mut l, n);
        l.push(flags);
        varint::put(&mut l, line);
        varint::put(&mut l, exc.len() as u64);
        let mut last = 0;
        for &(i, k) in exc {
            varint::put(&mut l, (i - last) << 1 | k as u64);
            last = i;
        }
    }
    l
}

#[test]
fn layouts_that_are_wrong_are_errors() {
    let data = b"caf\xc3\xa9 word word\n".to_vec();
    let run = |l: &[u8], data: &[u8], len: u64| formula::decode(&[], &[b"ab", data, l], len);
    // right: `caf=C3=A9 word word\n` in a text of 2 bytes, a gap of 1
    let good = layout(&[(1, data.len() as u64, 1, 76, &[])]);
    let out = run(&good, &data, 2 + 20).unwrap();
    assert_eq!(out, b"acaf=C3=A9 word word\nb");
    // exceptions: an escaped `w` (a flip at token 6), and a soft break before it
    let l = layout(&[(1, data.len() as u64, 1, 76, &[(6, 0), (6, 1)])]);
    assert_eq!(run(&l, &data, 2 + 20 + 3 - 1 + 2).unwrap(), b"acaf=C3=A9 =\n=77ord word\nb");
    let bad = |l: Vec<u8>, data: &[u8], len: u64| assert!(run(&l, data, len).is_err(), "{l:?}");
    bad(good.clone(), &data, 2 + 19);
    bad(good.clone(), &data[1..], 2 + 20);
    bad(good.clone(), &[&data[..], b"x"].concat(), 2 + 21);
    // flags past the four, a line shorter than 4 or past the most, no bytes, more than there are
    bad(layout(&[(1, 17, 16, 76, &[])]), &data, 22);
    bad(layout(&[(1, 17, 1, 3, &[])]), &data, 22);
    bad(layout(&[(1, 17, 1, (1 << 16) + 1, &[])]), &data, 22);
    bad(layout(&[(1, 0, 1, 76, &[])]), &data, 22);
    bad(layout(&[(1, 18, 1, 76, &[])]), &data, 22);
    // a gap longer than the text
    bad(layout(&[(3, 17, 1, 76, &[])]), &data, 22);
    // exceptions: past the data, out of order, doubled, a byte written as it is that cannot be
    bad(layout(&[(1, 17, 1, 76, &[(17, 0)])]), &data, 22);
    let mut l = Vec::new();
    varint::put(&mut l, 1);
    varint::put(&mut l, 17);
    l.push(1);
    varint::put(&mut l, 76);
    varint::put(&mut l, 2);
    varint::put(&mut l, 5 << 1 | 1);
    varint::put(&mut l, 0);
    bad(l.clone(), &data, 22);
    let mut l = layout(&[(1, 17, 1, 76, &[(5, 0)])]);
    *l.last_mut().unwrap() = 0;
    bad(l, &data, 22);
    bad(layout(&[(1, 17, 1, 76, &[(3, 0)])]).into_iter().take(5).collect(), &data, 22);
    // (a high byte flipped to literal)
    bad(layout(&[(1, 17, 1, 76, &[(3, 0)])]), &data, 22);
    // a layout that ends in a run, and none
    let mut cut = good.clone();
    cut.pop();
    bad(cut, &data, 22);
    bad(Vec::new(), &data, 22);
    assert!(formula::decode(&[0], &[b"ab", &data, &good], 22).is_err());
    assert!(formula::decode(&[], &[b"ab", &data], 22).is_err());
}

#[test]
fn random_layouts_never_panic() {
    let data = b"caf\xc3\xa9 word word\n".repeat(20);
    let mut rng = Rng::new(48);
    for _ in 0..3000 {
        let n = rng.below(40) as usize;
        let mut l: Vec<u8> = (0..n).map(|_| rng.below(256) as u8).collect();
        for b in &mut l {
            if rng.below(2) == 0 {
                *b %= 9;
            }
        }
        let len = rng.below(600);
        let _ = formula::decode(&[], &[b"some text", &data, &l], len);
    }
}

#[test]
fn a_body_is_the_part_module_too() {
    // (the same through the pieces of `build`, over inputs longer than a piece, exceptions among them)
    let enc = Enc { crlf: false, ..Enc::new() };
    let data = body(200_000, 49);
    let mut d = b"Content-Transfer-Encoding: quoted-printable\n\n".to_vec();
    d.extend(enc.encode(&data));
    d.extend_from_slice(b"\nFrom a:b\n");
    d.extend(body(70_000, 50));
    let f = taken(&d);
    assert_eq!(data_of(&f), data);
}
