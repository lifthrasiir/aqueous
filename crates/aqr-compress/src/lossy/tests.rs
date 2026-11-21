use super::*;

use std::io::Read;

fn crc32(d: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(d);
    c.sum()
}

/// Text of some size and structure: lines repeated with variations.
fn text(seed: u32, lines: usize) -> Vec<u8> {
    let mut s = String::new();
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    for i in 0..lines {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        s.push_str(&format!("line {i}: the quick {} fox jumps over {} lazy dogs\n", x >> 20, (x >> 8) & 0xff));
    }
    s.into_bytes()
}

/// A raw DEFLATE stream of an encoder that is not zlib.
fn other_deflate(data: &[u8]) -> Vec<u8> {
    "libdeflate:level=9".parse::<flate_zoo::Encoder>().unwrap().compress(data).unwrap()
}

/// A raw DEFLATE stream of zlib's level 6, which is kept as it is.
fn zlib_deflate(data: &[u8]) -> Vec<u8> {
    aqr_flate::zlib::compress(data, aqr_flate::zlib::Params::default()).unwrap()
}

struct Ent {
    name: &'static str,
    method: u16,
    data: Vec<u8>,
    body: Vec<u8>,
    /// A data descriptor (flag 3): signature, long.
    desc: Option<(bool, bool)>,
}

impl Ent {
    fn deflated(name: &'static str, data: Vec<u8>, body: Vec<u8>) -> Ent {
        Ent { name, method: 8, data, body, desc: None }
    }

    fn stored(name: &'static str, data: Vec<u8>) -> Ent {
        Ent { name, method: 0, body: data.clone(), data, desc: None }
    }

    fn desc(mut self, sig: bool, long: bool) -> Ent {
        self.desc = Some((sig, long));
        self
    }
}

#[derive(Default, Clone, Copy)]
struct Style {
    zip64: bool,
    /// Bytes between the last local entry and the central directory.
    pad: &'static [u8],
    comment: &'static str,
}

fn build(ents: &[Ent], st: Style) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    let le16 = |v: &mut Vec<u8>, n: u16| v.extend(n.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, n: u32| v.extend(n.to_le_bytes());
    let le64 = |v: &mut Vec<u8>, n: u64| v.extend(n.to_le_bytes());
    for e in ents {
        offsets.push(out.len() as u64);
        let flags: u16 = if e.desc.is_some() { 8 } else { 0 };
        let crc = crc32(&e.data);
        out.extend(b"PK\x03\x04");
        le16(&mut out, 20);
        le16(&mut out, flags);
        le16(&mut out, e.method);
        le16(&mut out, 0x6000);
        le16(&mut out, 0x5821);
        let (c, cs, us) = if e.desc.is_some() { (0, 0, 0) } else { (crc, e.body.len() as u32, e.data.len() as u32) };
        le32(&mut out, c);
        if st.zip64 {
            le32(&mut out, u32::MAX);
            le32(&mut out, u32::MAX);
        } else {
            le32(&mut out, cs);
            le32(&mut out, us);
        }
        le16(&mut out, e.name.len() as u16);
        le16(&mut out, if st.zip64 { 20 } else { 0 });
        out.extend(e.name.as_bytes());
        if st.zip64 {
            le16(&mut out, 1);
            le16(&mut out, 16);
            le64(&mut out, e.data.len() as u64);
            le64(&mut out, e.body.len() as u64);
        }
        out.extend(&e.body);
        if let Some((sig, long)) = e.desc {
            if sig {
                out.extend(b"PK\x07\x08");
            }
            le32(&mut out, crc);
            if long {
                le64(&mut out, e.body.len() as u64);
                le64(&mut out, e.data.len() as u64);
            } else {
                le32(&mut out, e.body.len() as u32);
                le32(&mut out, e.data.len() as u32);
            }
        }
    }
    out.extend(st.pad);
    let cd = out.len() as u64;
    for (e, &off) in ents.iter().zip(&offsets) {
        let flags: u16 = if e.desc.is_some() { 8 } else { 0 };
        out.extend(b"PK\x01\x02");
        le16(&mut out, 0x031e);
        le16(&mut out, 20);
        le16(&mut out, flags);
        le16(&mut out, e.method);
        le16(&mut out, 0x6000);
        le16(&mut out, 0x5821);
        le32(&mut out, crc32(&e.data));
        if st.zip64 {
            le32(&mut out, u32::MAX);
            le32(&mut out, u32::MAX);
        } else {
            le32(&mut out, e.body.len() as u32);
            le32(&mut out, e.data.len() as u32);
        }
        le16(&mut out, e.name.len() as u16);
        le16(&mut out, if st.zip64 { 28 } else { 0 });
        le16(&mut out, 3);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le32(&mut out, 0o100644 << 16);
        le32(&mut out, if st.zip64 { u32::MAX } else { off as u32 });
        out.extend(e.name.as_bytes());
        if st.zip64 {
            le16(&mut out, 1);
            le16(&mut out, 24);
            le64(&mut out, e.data.len() as u64);
            le64(&mut out, e.body.len() as u64);
            le64(&mut out, off);
        }
        out.extend(b"cmt");
    }
    let cd_size = out.len() as u64 - cd;
    if st.zip64 {
        let at = out.len() as u64;
        out.extend(b"PK\x06\x06");
        le64(&mut out, 44);
        le16(&mut out, 45);
        le16(&mut out, 45);
        le32(&mut out, 0);
        le32(&mut out, 0);
        le64(&mut out, ents.len() as u64);
        le64(&mut out, ents.len() as u64);
        le64(&mut out, cd_size);
        le64(&mut out, cd);
        out.extend(b"PK\x06\x07");
        le32(&mut out, 0);
        le64(&mut out, at);
        le32(&mut out, 1);
    }
    out.extend(b"PK\x05\x06");
    le16(&mut out, 0);
    le16(&mut out, 0);
    let n = if st.zip64 { 0xffff } else { ents.len() as u16 };
    le16(&mut out, n);
    le16(&mut out, n);
    le32(&mut out, if st.zip64 { u32::MAX } else { cd_size as u32 });
    le32(&mut out, if st.zip64 { u32::MAX } else { cd as u32 });
    le16(&mut out, st.comment.len() as u16);
    out.extend(st.comment.as_bytes());
    out
}

fn entries() -> Vec<Ent> {
    let (a, b, c) = (text(1, 600), text(2, 900), text(3, 300));
    vec![
        Ent::deflated("a.txt", a.clone(), other_deflate(&a)),
        Ent::stored("stored.bin", text(4, 50)),
        // (zlib's: kept as it is)
        Ent::deflated("z.txt", c.clone(), zlib_deflate(&c)),
        Ent::deflated("dir/b.txt", b.clone(), other_deflate(&b)),
    ]
}

/// The data of the entries of `zip` as aqr-zip reads them, inflated.
fn contents(zip: &[u8]) -> Vec<Vec<u8>> {
    let (_, _, recs) = aqr_zip::zip::records(zip);
    recs.iter()
        .filter_map(|r| match r {
            aqr_zip::zip::Rec::Local(l) => {
                let body = &zip[l.body.clone()];
                Some(if l.method(zip) == 0 {
                    body.to_vec()
                } else {
                    let mut v = Vec::new();
                    flate2::read::DeflateDecoder::new(body).read_to_end(&mut v).unwrap();
                    v
                })
            }
            _ => None,
        })
        .collect()
}

fn rewritten(x: &[u8]) -> Vec<u8> {
    let r = zip::coding(x, &high()).unwrap().expect("a stream was rewritten");
    assert_ne!(r, x);
    r
}

fn round(ents: &[Ent], st: Style) {
    let x = build(ents, st);
    let r = rewritten(&x);
    check::zip(&x, &r).unwrap();
    let want: Vec<Vec<u8>> = ents.iter().map(|e| e.data.clone()).collect();
    assert_eq!(contents(&x), want);
    assert_eq!(contents(&r), want);
}

#[test]
fn plain() {
    round(&entries(), Style::default());
}

#[test]
fn comment_and_descriptors() {
    let mut e = entries();
    e[0] = std::mem::replace(&mut e[0], Ent::stored("", vec![])).desc(true, false);
    e[3] = std::mem::replace(&mut e[3], Ent::stored("", vec![])).desc(false, false);
    round(&e, Style { comment: "an archive comment", ..Style::default() });
    let mut e = entries();
    e[0] = std::mem::replace(&mut e[0], Ent::stored("", vec![])).desc(true, true);
    e[3] = std::mem::replace(&mut e[3], Ent::stored("", vec![])).desc(false, true);
    // (aqr-zip reads the long form only where the local header has zip64 data)
    round(&e, Style { zip64: true, ..Style::default() });
}

#[test]
fn zip64() {
    round(&entries(), Style { zip64: true, comment: "z64", ..Style::default() });
}

#[test]
fn nothing_to_change() {
    let c = text(3, 300);
    let e = [Ent::deflated("z.txt", c.clone(), zlib_deflate(&c)), Ent::stored("s", c)];
    assert_eq!(zip::coding(&build(&e, Style::default()), &high()), Ok(None));
    assert_eq!(zip::coding(&build(&[], Style::default()), &high()), Ok(None));
}

#[test]
fn identical_passes() {
    let x = build(&entries(), Style::default());
    check::zip(&x, &x).unwrap();
}

#[test]
fn tampering_is_found() {
    let ents = entries();
    let x = build(&ents, Style::default());
    let r = rewritten(&x);
    check::zip(&x, &r).unwrap();
    let find = |d: &[u8], s: &[u8]| d.windows(s.len()).position(|w| w == s).unwrap();
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // a name (the central header's, then the local's)
    let mut t = r.clone();
    let at = find(&t, b"dir/b.txt");
    t[at] = b'D';
    cases.push(("local name", t));
    let mut t = r.clone();
    let at = find(&t, b"PK\x01\x02") + 46;
    t[at] = b'x';
    cases.push(("central name", t));
    // the data of a deflated entry, of the stored one
    let mut t = r.clone();
    let at = find(&t, b"PK\x03\x04") + 30 + 5 + 40;
    t[at] ^= 0x10;
    cases.push(("deflated data", t));
    let mut t = r.clone();
    let stored = &ents[1].data[..32];
    let at = find(&t, stored) + 7;
    t[at] ^= 1;
    cases.push(("stored data", t));
    // the comment, the end record's offset
    let mut t = r.clone();
    let n = t.len();
    t[n - 1] ^= 1;
    cases.push(("comment", t.clone()));
    let mut t = r.clone();
    let n = t.len();
    t[n - 6] ^= 1;
    cases.push(("directory offset", t));
    // the CRC-32 in a central header
    let mut t = r.clone();
    let at = find(&t, b"PK\x01\x02") + 16;
    t[at] ^= 1;
    cases.push(("crc", t));
    // the original as it was, though sizes changed in R: a size not of its body
    let mut t = r.clone();
    let at = find(&t, b"PK\x01\x02") + 20;
    t[at] ^= 1;
    cases.push(("compressed size", t));
    for (what, t) in cases {
        assert!(check::zip(&x, &t).is_err(), "{what} not found");
    }
}

#[test]
fn turned_down() {
    let ents = entries();
    let x = build(&ents, Style::default());
    // a prefix
    let mut p = b"MZ stub".to_vec();
    p.extend(&x);
    assert!(zip::coding(&p, &high()).is_err());
    // encrypted (flag 0 of the first local and central headers)
    let mut t = x.clone();
    let at = t.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    t[6] |= 1;
    t[at + 8] |= 1;
    assert_eq!(zip::coding(&t, &high()), Err("encrypted".into()));
    // another method
    let mut t = x.clone();
    let at = t.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    t[8] = 12;
    t[at + 10] = 12;
    assert!(zip::coding(&t, &high()).is_err());
    // signed
    let mut e = entries();
    e.push(Ent::stored("AppxSignature.p7x", vec![1, 2, 3]));
    let r = zip::coding(&build(&e, Style::default()), &high());
    assert_eq!(r, Err("signed (MSIX/AppX)".into()));
    let pad = b"\x20\0\0\0\0\0\0\0signing block\x20\0\0\0\0\0\0\0APK Sig Block 42";
    let r = zip::coding(&build(&entries(), Style { pad, ..Style::default() }), &high());
    assert_eq!(r, Err("signed (APK signing block)".into()));
    // padding that is no signing block
    let r = zip::coding(&build(&entries(), Style { pad: b"padding.", ..Style::default() }), &high());
    assert!(r.is_err());
}

/// Options of level 8, where aqr-flate emulates zlib (`coding` of DEFLATE is from level 7 on).
pub(super) fn high() -> Options {
    Options { level: aqr_archive::Level::HIGH, ..Options::default() }
}
