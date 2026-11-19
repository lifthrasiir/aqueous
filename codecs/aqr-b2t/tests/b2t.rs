//! Text with base64 blocks through the formula and the module: the layouts of mail, PEM, LDIF,
//! XML and JSON, both alphabets, padded and not, and what is not taken (hex strings and
//! identifiers, blocks written otherwise than the module writes them); layouts that are not
//! right turned down.

use aqr_archive::Evaluate;
use aqr_b2t::{formula, module};
use aqr_common::rng::Rng;
use aqr_formula::{Apply, Formula};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

fn b64(d: &[u8], url: bool, pad: bool) -> Vec<u8> {
    let t = if url { module::URL_SAFE } else { module::STANDARD };
    let mut out = Vec::new();
    for g in d.chunks(3) {
        let v = (g[0] as u32) << 16 | (*g.get(1).unwrap_or(&0) as u32) << 8 | *g.get(2).unwrap_or(&0) as u32;
        let k = g.len() + 1;
        for i in 0..k {
            out.push(t[(v >> (18 - 6 * i) & 63) as usize]);
        }
        if pad {
            out.resize(out.len() + 4 - k, b'=');
        }
    }
    out
}

/// `chars` cut into a first line of `first` and lines of `line`, the lines joined by `sep`.
fn wrap(chars: &[u8], first: usize, line: usize, sep: &[u8]) -> Vec<u8> {
    let mut out = chars[..first.min(chars.len())].to_vec();
    for l in chars[first.min(chars.len())..].chunks(line) {
        out.extend_from_slice(sep);
        out.extend_from_slice(l);
    }
    out
}

fn payload(n: usize, seed: u64) -> Vec<u8> {
    Rng::new(seed).bytes(n)
}

fn words(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let w: [&[u8]; 6] = [b"the ", b"mail ", b"of ", b"quick ", b"fox\n", b"attachment "];
    let mut t = Vec::new();
    while t.len() < n {
        t.extend_from_slice(w[rng.below(6) as usize]);
    }
    t
}

/// The streams of `d`'s formula, and that it gives `d` back.
fn taken(d: &[u8]) -> Formula {
    let f = formula::encode(d, 1).expect("a block taken");
    assert_eq!(eval(&f).unwrap(), d);
    f
}

fn data_of(f: &Formula) -> Vec<u8> {
    f.streams.iter().find(|s| s.class == "b2t/data").unwrap().data.to_vec()
}

fn not_taken(d: &[u8]) {
    if let Some(f) = formula::encode(d, 1) {
        // (a block taken is written again as it was)
        assert_eq!(eval(&f).unwrap(), d);
        panic!("taken: {} bytes of data", data_of(&f).len());
    }
}

#[test]
fn mime_crlf_76() {
    for n in [1000, 1001, 1002, 3000] {
        let p = payload(n, 1);
        let mut d = b"From: a@b\r\nContent-Transfer-Encoding: base64\r\n\r\n".to_vec();
        d.extend(wrap(&b64(&p, false, true), 76, 76, b"\r\n"));
        d.extend_from_slice(b"\r\n\r\n--boundary--\r\n");
        assert_eq!(data_of(&taken(&d)), p, "{n}");
    }
}

#[test]
fn lf_76_and_pem_64() {
    let p = payload(2000, 2);
    let mut d = words(300, 1);
    d.extend_from_slice(b"-----BEGIN CERTIFICATE-----\n");
    d.extend(wrap(&b64(&p, false, true), 64, 64, b"\n"));
    d.extend_from_slice(b"\n-----END CERTIFICATE-----\n");
    assert_eq!(data_of(&taken(&d)), p);
    let mut d = b"Content-Transfer-Encoding: base64\n\n".to_vec();
    d.extend(wrap(&b64(&p, false, true), 76, 76, b"\n"));
    d.extend_from_slice(b"\nplain text again\n");
    assert_eq!(data_of(&taken(&d)), p);
}

#[test]
fn ldif_folding_with_a_shorter_first_line() {
    let p = payload(1500, 3);
    let mut d = b"dn: cn=x\njpegPhoto:: ".to_vec();
    d.extend(wrap(&b64(&p, false, true), 64, 75, b"\n "));
    d.extend_from_slice(b"\nmail: x@y\n");
    let f = taken(&d);
    assert_eq!(data_of(&f), p);
}

#[test]
fn xml_indented() {
    let p = payload(1800, 4);
    let mut d = b"<root>\n  <data encoding=\"base64\">\n      ".to_vec();
    d.extend(wrap(&b64(&p, false, true), 60, 60, b"\n      "));
    d.extend_from_slice(b"\n  </data>\n</root>\n");
    assert_eq!(data_of(&taken(&d)), p);
}

#[test]
fn json_escaped_lines() {
    let p = payload(1800, 5);
    for sep in [&b"\\n"[..], b"\\r\\n"] {
        let mut d = b"{\"cells\": [{\"data\": {\"image/png\": \"".to_vec();
        d.extend(wrap(&b64(&p, false, true), 76, 76, sep));
        d.extend_from_slice(sep);
        d.extend_from_slice(b"\"}}]}\n");
        assert_eq!(data_of(&taken(&d)), p, "{sep:?}");
        // (the escape at the start of it)
        let mut d = b"{\"a\": \"".to_vec();
        d.extend_from_slice(sep);
        d.extend(wrap(&b64(&p, false, true), 76, 76, sep));
        d.extend_from_slice(b"\"}\n");
        assert_eq!(data_of(&taken(&d)), p, "{sep:?} first");
    }
}

#[test]
fn unwrapped_data_uris_and_adjacent_runs() {
    let (p, q) = (payload(5000, 6), payload(700, 7));
    let mut d = b"<img src=\"data:image/png;base64,".to_vec();
    d.extend(b64(&p, false, true));
    d.extend_from_slice(b"\"> <img src=\"data:image/png;base64,");
    d.extend(b64(&q, false, true));
    d.extend_from_slice(b"\">");
    let f = taken(&d);
    assert_eq!(data_of(&f), [p.clone(), q.clone()].concat());
    // (adjacent: a separator between them that is no character of the alphabet)
    let mut d = b"[\"".to_vec();
    d.extend(b64(&p, false, true));
    d.extend_from_slice(b"\",\"");
    d.extend(b64(&q, false, true));
    d.extend_from_slice(b"\"]");
    assert_eq!(data_of(&taken(&d)), [p, q].concat());
}

#[test]
fn url_safe_and_unpadded() {
    for n in [1000, 1001, 1002] {
        let p = payload(n, 8);
        // (every byte pattern with - or _ in it)
        let chars = b64(&p, true, false);
        assert!(chars.iter().any(|&c| c == b'-' || c == b'_'));
        let mut d = b"{\"k\":\"".to_vec();
        d.extend(&chars);
        d.extend_from_slice(b"\"}");
        let f = taken(&d);
        assert_eq!(data_of(&f), p, "{n}");
        let mut d = b"x: ".to_vec();
        d.extend(wrap(&b64(&p, true, false), 64, 64, b"\n"));
        d.extend_from_slice(b"\n");
        assert_eq!(data_of(&taken(&d)), p, "wrapped {n}");
        // (standard, unpadded)
        let mut d = b"x: ".to_vec();
        d.extend(wrap(&b64(&p, false, false), 64, 64, b"\n"));
        d.extend_from_slice(b"\n");
        assert_eq!(data_of(&taken(&d)), p, "std {n}");
    }
}

#[test]
fn start_and_end_of_the_file() {
    let p = payload(900, 9);
    let c = wrap(&b64(&p, false, true), 76, 76, b"\n");
    assert_eq!(data_of(&taken(&c)), p);
    let mut d = c.clone();
    d.extend_from_slice(b"\n tail");
    assert_eq!(data_of(&taken(&d)), p);
    let mut d = b"head\n".to_vec();
    d.extend(&c);
    assert_eq!(data_of(&taken(&d)), p);
}

#[test]
fn non_canonical_bits_are_not_taken_or_written_as_they_were() {
    let p = payload(1000, 10);
    // 1000 bytes = 1334 chars with 2 padding: the last char's low bits set
    let mut c = b64(&p, false, true);
    let i = c.len() - 3;
    let t = module::STANDARD;
    c[i] = t[t.iter().position(|&x| x == c[i]).unwrap() | 1];
    let mut d = b"x ".to_vec();
    d.extend(wrap(&c, 76, 76, b"\n"));
    d.extend_from_slice(b"\n");
    if let Some(f) = formula::encode(&d, 1) {
        assert_eq!(eval(&f).unwrap(), d);
    }
}

#[test]
fn not_base64_but_hex() {
    let mut rng = Rng::new(11);
    let hex: Vec<u8> = (0..2000).map(|_| b"0123456789abcdef"[rng.below(16) as usize]).collect();
    // (hexadecimal is of its own alphabet, never base64's: 2 and 3 of the layout's flags)
    let f = taken(&wrap(&hex, 64, 64, b"\n"));
    assert_eq!(f.streams.iter().find(|s| s.class == "b2t/layout").unwrap().data.last().copied(), Some(4));
    // (a single line without the prefix of a literal)
    not_taken(&wrap(&hex, 0, 2000, b"\n"));
    let ident = b"ThisIsAVeryLongCamelCaseIdentifierWithoutAnyDigitsInItAtAll".repeat(30);
    not_taken(&ident);
    let lower: Vec<u8> = (0..3000).map(|_| b'a' + rng.below(26) as u8).collect();
    not_taken(&wrap(&lower, 76, 76, b"\n"));
    not_taken(&words(5000, 3));
    // (shorter than the least)
    let p = payload(150, 12);
    let mut d = b"x ".to_vec();
    d.extend(b64(&p, false, true));
    not_taken(&d);
}

fn layout(seps: &[&[u8]], runs: &[(u64, u64, u64, u64, u64, u8)]) -> Vec<u8> {
    use aqr_common::varint::put;
    let mut l = Vec::new();
    put(&mut l, seps.len() as u64);
    for s in seps {
        put(&mut l, s.len() as u64);
        l.extend_from_slice(s);
    }
    for r in runs {
        for v in [r.0, r.1, r.2, r.3, r.4] {
            put(&mut l, v);
        }
        l.push(r.5);
    }
    l
}

#[test]
fn layouts_that_are_wrong_are_errors() {
    let data = payload(100, 13);
    let text = b"ab";
    let run = |l: &[u8], text: &[u8], data: &[u8], len: u64| formula::decode(&[], &[text, data, l], len);
    // right: 100 bytes, 136 chars (padded: 4 * 34), lines of 76
    let good = layout(&[b"\n"], &[(1, 100, 76, 76, 0, 1)]);
    let out = run(&good, text, &data, 2 + 136 + 1).unwrap();
    assert_eq!(&out[..1], b"a");
    assert_eq!(out[1 + 76], b'\n');
    assert_eq!(*out.last().unwrap(), b'b');
    // the same with the lengths wrong
    assert!(run(&good, text, &data, 2 + 136).is_err());
    assert!(run(&good, text, &data[..99], 2 + 136 + 1).is_err());
    assert!(run(&good, text, &[&data[..], b"x"].concat(), 2 + 136 + 1).is_err());
    assert!(run(&good, b"a", &data, 2 + 136 + 1).is_err());
    assert!(run(&good, b"abc", &data, 2 + 136 + 1).is_err());
    assert!(formula::decode(&[0], &[text, &data, &good], 2 + 136 + 1).is_err());
    assert!(formula::decode(&[], &[text, &data], 2 + 136 + 1).is_err());
    let bad = |l: Vec<u8>| assert!(run(&l, text, &data, 2 + 136 + 1).is_err(), "{l:?}");
    // a separator not there, one too long, one empty, too many
    bad(layout(&[b"\n"], &[(1, 100, 76, 76, 1, 1)]));
    bad(layout(&[&[b'x'; 33][..]], &[(1, 100, 76, 76, 0, 1)]));
    bad(layout(&[b""], &[(1, 100, 76, 76, 0, 1)]));
    bad(layout(&[&b"\n"[..]; 65], &[(1, 100, 76, 76, 0, 1)]));
    // lines: a first line with none after, one past the block, an unwrapped run with lines
    bad(layout(&[b"\n"], &[(1, 100, 76, 0, 0, 1)]));
    bad(layout(&[b"\n"], &[(1, 100, 136, 76, 0, 1)]));
    bad(layout(&[b"\n"], &[(1, 100, 0, 76, 0, 1)]));
    bad(layout(&[b"\n"], &[(1, 100, 0, 0, 1, 1)]));
    // flags: an alphabet that is not, bits that are not
    bad(layout(&[b"\n"], &[(1, 100, 76, 76, 0, 5)]));
    bad(layout(&[b"\n"], &[(1, 100, 76, 76, 0, 0x11)]));
    // no bytes in a run, more than there are, a layout that ends in a run
    bad(layout(&[b"\n"], &[(1, 0, 76, 76, 0, 1)]));
    bad(layout(&[b"\n"], &[(1, 101, 76, 76, 0, 1)]));
    let mut cut = good.clone();
    cut.pop();
    bad(cut);
    bad(Vec::new());
    // (a gap longer than the text)
    bad(layout(&[b"\n"], &[(3, 100, 76, 76, 0, 1)]));
    // unwrapped, and with no separators at all
    let l = layout(&[], &[(0, 100, 0, 0, 0, 1)]);
    assert_eq!(run(&l, text, &data, 2 + 136).unwrap().len(), 138);
}

#[test]
fn random_layouts_never_panic() {
    let data = payload(300, 14);
    let mut rng = Rng::new(15);
    for _ in 0..3000 {
        let n = rng.below(40) as usize;
        let mut l: Vec<u8> = (0..n).map(|_| rng.below(256) as u8).collect();
        // (small numbers are likelier to get far)
        for b in &mut l {
            if rng.below(2) == 0 {
                *b %= 6;
            }
        }
        let len = rng.below(600);
        let _ = formula::decode(&[], &[b"some text", &data, &l], len);
    }
}

#[test]
fn a_run_is_the_part_module_too() {
    // (the same through the pieces of `build`, over inputs longer than a piece)
    let p = payload(300_000, 16);
    let mut d = words(100_000, 2);
    d.extend(wrap(&b64(&p, false, true), 76, 76, b"\r\n"));
    d.extend_from_slice(b"\r\n");
    d.extend(words(70_000, 3));
    let f = taken(&d);
    assert_eq!(data_of(&f), p);
}

#[test]
fn blocks_of_a_small_share_of_the_text_are_not_taken() {
    // (a GB of Wikipedia with a few blocks of a KB: the text would be copied for nothing)
    let mut d = words(200_000, 7);
    d.extend_from_slice(b"\n");
    d.extend_from_slice(&wrap(&b64(&payload(1000, 8), false, true), 76, 76, b"\n"));
    d.extend_from_slice(b"\n");
    d.extend_from_slice(&words(200_000, 9));
    not_taken(&d);
}

#[test]
fn lines_shorter_than_an_encoders_are_not_a_block() {
    // (a long line of the URL-safe alphabet, then `%2520` again and again: enwik9's URL of a
    // review, taken as a block of lines of 4 split by `%`)
    let chars = b64(&payload(660, 10), true, false);
    let mut d = words(100, 11);
    d.extend_from_slice(b" http://x/?q=");
    d.extend_from_slice(&chars[..880]);
    (0..60).for_each(|_| d.extend_from_slice(b"%2520"));
    d.extend_from_slice(b" ");
    d.extend_from_slice(&words(100, 12));
    if let Some(f) = formula::encode(&d, 1) {
        assert_eq!(eval(&f).unwrap(), d);
        let layout = &f.streams.iter().find(|s| s.class == "b2t/layout").unwrap().data;
        // (no separator: the block, where taken, is the first line alone)
        assert_eq!(layout[0], 0, "wrapped at lines of 4");
    }
}

#[test]
fn mangled_names_between_nuls_are_not_blocks() {
    // (a string table of libstdc++: long C++ names of the URL-safe alphabet's characters, `_`
    // among them, one after another between NULs)
    let mut d = Vec::new();
    for i in 0..200 {
        d.extend_from_slice(b"_ZNKSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE");
        d.extend_from_slice(format!("{i}St8iteratorISt26random_access_iterator_tagIcEiPcRcE").as_bytes());
        d.extend_from_slice(b"N9__gnu_cxx17__normal_iteratorIPKcNSt7__cxx1112basic_stringIcEEEE");
        d.extend_from_slice(b"S0_ISt9allocatorIcEE15_M_replace_auxEjjjc12ZZ34nested_lambda_typeIvE5Impl3run");
        d.push(0);
    }
    not_taken(&d);
}

// ---- hexadecimal (alphabets 2 and 3)

fn hex(d: &[u8], upper: bool) -> Vec<u8> {
    let t = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    d.iter().flat_map(|&b| [t[(b >> 4) as usize], t[(b & 15) as usize]]).collect()
}

/// Bytes whose hex has digits and letters of both cases' alphabets alike.
fn bytes_for_hex(n: usize, seed: u64) -> Vec<u8> {
    payload(n, seed)
}

#[test]
fn rtf_pict_lines_of_128() {
    for (n, sep) in [(1500usize, &b"\r\n"[..]), (1501, b"\n"), (640, b"\r\n")] {
        let p = bytes_for_hex(n, 20);
        let mut d = b"{\\rtf1\\ansi{\\pict\\pngblip\\picw100\\pich100\r\n".to_vec();
        d.extend(wrap(&hex(&p, false), 128, 128, sep));
        d.extend_from_slice(b"}}\r\n");
        assert_eq!(data_of(&taken(&d)), p, "{n}");
    }
}

#[test]
fn single_line_hex_with_a_prefix() {
    let p = bytes_for_hex(1200, 21);
    // mysqldump --hex-blob: uppercase after 0x
    let mut d = b"INSERT INTO `t` VALUES (1,0x".to_vec();
    d.extend(hex(&p, true));
    d.extend_from_slice(b"),(2,NULL);\n");
    assert_eq!(data_of(&taken(&d)), p);
    // PostgreSQL bytea
    let mut d = b"INSERT INTO t VALUES (1, '\\x".to_vec();
    d.extend(hex(&p, false));
    d.extend_from_slice(b"');\n");
    assert_eq!(data_of(&taken(&d)), p);
    // SQLite .dump
    let mut d = b"INSERT INTO t VALUES(1,X'".to_vec();
    d.extend(hex(&p, true));
    d.extend_from_slice(b"');\n");
    assert_eq!(data_of(&taken(&d)), p);
    // COPY's text format: a doubled backslash, tab delimited
    let mut d = b"COPY t (a, b) FROM stdin;\n1\t\\\\x".to_vec();
    d.extend(hex(&p, false));
    d.extend_from_slice(b"\n\\.\n");
    assert_eq!(data_of(&taken(&d)), p);
    // (no prefix: a long hexadecimal string alone is not taken)
    let mut d = b"INSERT INTO t VALUES (1, '".to_vec();
    d.extend(hex(&p, false));
    d.extend_from_slice(b"');\n");
    not_taken(&d);
}

#[test]
fn postscript_and_xxd_lines() {
    let p = bytes_for_hex(3000, 22);
    let mut d =
        b"%!PS-Adobe-3.0\n/pic 100 string def\n100 100 8 [1 0 0 1 0 0]\n{currentfile pic readhexstring pop}\nimage\n"
            .to_vec();
    d.extend(wrap(&hex(&p, true), 78, 78, b"\n"));
    d.extend_from_slice(b"\nshowpage\n");
    assert_eq!(data_of(&taken(&d)), p);
    for line in [60usize, 64, 80] {
        let mut d = wrap(&hex(&p, false), line, line, b"\n");
        d.push(b'\n');
        assert_eq!(data_of(&taken(&d)), p, "xxd -p {line}");
    }
}

#[test]
fn odd_hex_gives_its_last_char_back() {
    let p = bytes_for_hex(400, 23);
    let mut c = hex(&p, false);
    c.push(b'7');
    let mut d = b"data\n".to_vec();
    d.extend(wrap(&c, 64, 64, b"\n"));
    d.extend_from_slice(b"\nend\n");
    let f = taken(&d);
    assert_eq!(data_of(&f), p);
}

#[test]
fn mixed_case_and_digits_alone_are_not_hex() {
    let mut rng = Rng::new(24);
    let mixed: Vec<u8> = (0..4000).map(|_| b"0123456789abcdefABCDEF"[rng.below(22) as usize]).collect();
    not_taken(&wrap(&mixed, 64, 64, b"\n"));
    let digits: Vec<u8> = (0..4000).map(|_| b'0' + rng.below(10) as u8).collect();
    not_taken(&wrap(&digits, 64, 64, b"\n"));
    let mut d = b"x = 0x".to_vec();
    d.extend(&digits);
    d.extend_from_slice(b";\n");
    not_taken(&d);
    let letters: Vec<u8> = (0..4000).map(|_| b"abcdef"[rng.below(6) as usize]).collect();
    not_taken(&wrap(&letters, 64, 64, b"\n"));
}

#[test]
fn a_list_of_hashes_is_harmless() {
    // (one hash a line: wrapped hex, taken or not, it is written again)
    let mut rng = Rng::new(25);
    let mut d = Vec::new();
    for _ in 0..200 {
        d.extend(hex(&rng.bytes(32), false));
        d.push(b'\n');
    }
    if let Some(f) = formula::encode(&d, 1) {
        assert_eq!(eval(&f).unwrap(), d);
    }
}

#[test]
fn hex_layouts_that_are_wrong_are_errors() {
    let data = payload(100, 26);
    let run = |l: &[u8]| formula::decode(&[], &[b"ab", &data, l], 2 + 200 + 2);
    // right: 100 bytes, 200 chars, lines of 128, flags 4 (lowercase) and 6 (uppercase)
    for flags in [4u8, 6] {
        let out = run(&layout(&[b"\r\n"], &[(1, 100, 128, 128, 0, flags)])).unwrap();
        assert_eq!(out.len(), 204);
        assert_eq!(&out[1..1 + 200 - 72], &hex(&data, flags == 6)[..128]);
        assert_eq!(out[1 + 128], b'\r');
    }
    let bad = |l: Vec<u8>| assert!(run(&l).is_err(), "{l:?}");
    // padded hex, an alphabet past the hex ones, hex with the bits of 8
    bad(layout(&[b"\r\n"], &[(1, 100, 128, 128, 0, 5)]));
    bad(layout(&[b"\r\n"], &[(1, 100, 128, 128, 0, 7)]));
    bad(layout(&[b"\r\n"], &[(1, 100, 128, 128, 0, 8)]));
    bad(layout(&[b"\r\n"], &[(1, 100, 128, 128, 0, 12)]));
    // a first line past the chars (200), unwrapped with lines
    bad(layout(&[b"\r\n"], &[(1, 100, 200, 128, 0, 4)]));
    bad(layout(&[b"\r\n"], &[(1, 100, 0, 128, 0, 4)]));
}

#[test]
fn a_prefix_does_not_make_hex_base64() {
    // (`0x` or `\x` and hexadecimal is a run of the base64 alphabet too, valid by its bytes'
    // luck: of every length)
    for n in 1200..1208 {
        let p = bytes_for_hex(n, 40 + n as u64);
        for (head, upper, tail) in [
            (&b"INSERT INTO t VALUES (1,0x"[..], true, &b");\n"[..]),
            (b"INSERT INTO t VALUES (1,0x", false, b");\n"),
            (b"INSERT INTO t VALUES (1, '\\x", false, b"');\n"),
            (b"1\tname\t\\\\x", false, b"\n"),
        ] {
            let mut d = head.to_vec();
            d.extend(hex(&p, upper));
            d.extend_from_slice(tail);
            assert!(data_of(&taken(&d)) == p, "{n} {head:?}");
            // (the layout's flags: hexadecimal)
            let flags = *f_layout(&d).last().unwrap();
            assert!(flags == 4 || flags == 6, "{n} {head:?}: flags {flags}");
        }
    }
}

fn f_layout(d: &[u8]) -> Vec<u8> {
    let f = formula::encode(d, 1).unwrap();
    f.streams.iter().find(|s| s.class == "b2t/layout").unwrap().data.to_vec()
}

/// A block of `p` wrapped at `line` with the separator `sep` between its lines, in a mail.
fn block_with(p: &[u8], line: usize, sep: &[u8]) -> Vec<u8> {
    let mut d = b"Content-Transfer-Encoding: base64\n\n".to_vec();
    d.extend(wrap(&b64(p, false, true), line, line, sep));
    d.extend_from_slice(b"\nplain text again\n");
    d
}

#[test]
fn separators_of_the_most_bytes() {
    // 32 bytes a separator is the most, 33 are not taken (as a whole, or alone in a file)
    let p = payload(2000, 20);
    for n in [1usize, 2, 31, 32] {
        let sep = [&b"\n"[..], &b" ".repeat(n - 1)].concat();
        assert_eq!(data_of(&taken(&block_with(&p, 60, &sep))), p, "{n}");
    }
    for n in [33usize, 34, 70] {
        let sep = [&b"\n"[..], &b" ".repeat(n - 1)].concat();
        not_taken(&block_with(&p, 60, &sep));
    }
}

#[test]
fn more_than_64_distinct_separators() {
    // blocks each with a separator of its own: the first 64 are taken, the others left as text
    let mut d = words(300, 1);
    let mut all = Vec::new();
    for i in 0..100usize {
        let p = payload(600, 100 + i as u64);
        // (a distinct separator: i spaces of indentation, at most 31, and a tab pattern)
        let mut sep = b"\n".to_vec();
        sep.extend(std::iter::repeat_n(b' ', i % 30));
        sep.extend(std::iter::repeat_n(b'\t', i / 30));
        d.extend_from_slice(b"\nname: ");
        d.extend(wrap(&b64(&p, false, true), 48, 48, &sep));
        d.extend_from_slice(b"\n");
        d.extend(words(40, i as u64));
        all.push(p);
    }
    let f = taken(&d);
    let got = data_of(&f);
    assert!(!got.is_empty() && got.len() <= all.iter().map(|p| p.len()).sum::<usize>());
    // (the blocks taken are blocks of the file, in order)
    let mut rest = &got[..];
    for p in &all {
        if rest.starts_with(p) {
            rest = &rest[p.len()..];
        }
    }
    assert!(rest.is_empty());
    // exactly the 64th and the 65th: 64 distinct are taken, a 65th is not
    // (a separator of spaces and tabs by the bits of i: all different)
    let distinct = |i: usize| -> Vec<u8> {
        (0..7).fold(b"\n".to_vec(), |mut v, b| {
            v.push(if i >> b & 1 == 1 { b'\t' } else { b' ' });
            v
        })
    };
    let blocks = |n: usize| {
        let mut d = words(300, 1);
        for i in 0..n {
            d.extend_from_slice(b"\nname: ");
            d.extend(wrap(&b64(&payload(600, 7 + i as u64), false, true), 48, 48, &distinct(i)));
            d.push(b'\n');
            d.extend(words(40, i as u64));
        }
        d
    };
    assert_eq!(data_of(&taken(&blocks(64))).len(), 64 * 600);
    assert_eq!(data_of(&taken(&blocks(65))).len(), 64 * 600);
}

#[test]
fn mixed_line_ends_in_a_run() {
    let p = payload(2000, 21);
    let chars = b64(&p, false, true);
    for (what, seps) in [
        ("lf then crlf", vec![&b"\n"[..], b"\r\n"]),
        ("alternating", vec![b"\r\n", b"\n", b"\r\n", b"\n"]),
        ("a lone cr", vec![b"\n", b"\r", b"\n"]),
        ("lf, one different", vec![b"\n", b"\n", b"\n", b"\n \n"]),
    ] {
        let mut d = b"Content-Transfer-Encoding: base64\n\n".to_vec();
        for (i, l) in chars.chunks(76).enumerate() {
            if i > 0 {
                d.extend_from_slice(seps[(i - 1) % seps.len()]);
            }
            d.extend_from_slice(l);
        }
        d.extend_from_slice(b"\nplain text again\n");
        // (taken in part or left alone: the text back either way)
        if let Some(f) = formula::encode(&d, 1) {
            assert_eq!(eval(&f).unwrap(), d, "{what}");
        }
    }
}

#[test]
fn damaged_streams_and_params() {
    let p = payload(1500, 22);
    let mut d = words(200, 3);
    d.extend_from_slice(b"-----BEGIN X-----\n");
    d.extend(wrap(&b64(&p, false, true), 64, 64, b"\n"));
    d.extend_from_slice(b"\n-----END X-----\n");
    let f = taken(&d);
    let a = match &f.root {
        aqr_formula::Node::Apply(a) => a,
        _ => panic!("not an application"),
    };
    let datas: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    let limit = std::time::Duration::from_secs(20);
    for k in 0..datas.len() {
        aqr_testkit::damaged(&datas[k], 50, 64, limit, |v| {
            let inputs: Vec<&[u8]> = datas.iter().enumerate().map(|(i, s)| if i == k { v } else { &s[..] }).collect();
            let _ = formula::decode(&a.params, &inputs, a.len);
        });
    }
    let inputs: Vec<&[u8]> = datas.iter().map(|s| &s[..]).collect();
    aqr_testkit::damaged(&a.params, 50, 64, limit, |v| {
        let _ = formula::decode(v, &inputs, a.len);
    });
}
