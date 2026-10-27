use std::io::Write;

use aqr_common::rng::{Rng, noise};
use aqr_formula::{Formula, Node};
use aqr_zip::formula::{self, Options};
use aqr_zip::layout::{self, Body};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The module's output from the formula's inputs.
fn decode(f: &Formula, threads: usize) -> aqr_zip::Result<Vec<u8>> {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    formula::apply(a, &ins, threads)
}

/// Encodes `d`, checking that the module gives it back, and how the bodies are made.
fn roundtrip_with(d: &[u8], opt: &Options) -> (Formula, Vec<Body>) {
    let f = formula::encode_with(d, opt).expect("encoded").formula;
    for threads in [1, 4] {
        assert!(decode(&f, threads).unwrap() == d, "not rebuilt");
    }
    let bodies = layout::bodies(&f.streams[2].data).unwrap();
    (f, bodies)
}

fn roundtrip(d: &[u8]) -> Vec<Body> {
    roundtrip_with(d, &Options::default()).1
}

fn count(bodies: &[Body], b: Body) -> usize {
    bodies.iter().filter(|&&x| x == b).count()
}

fn text(n: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", i * 7 % 13).unwrap();
    }
    t.truncate(n);
    t
}

fn deflate(d: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

/// An entry of [`Zip`]: its name, its data, and how it is written.
#[derive(Clone)]
struct Entry {
    name: Vec<u8>,
    data: Vec<u8>,
    method: u16,
    flags: u16,
    /// The body, if not the data compressed by the method.
    body: Option<Vec<u8>>,
    /// The data descriptor: none, or with the signature and long sizes as given.
    desc: Option<(bool, bool)>,
    /// zip64 extra fields, local and central, with the 32-bit fields all ones.
    zip64: bool,
    local_extra: Vec<u8>,
    central_extra: Vec<u8>,
    comment: Vec<u8>,
}

fn entry(name: &str, data: Vec<u8>) -> Entry {
    Entry {
        name: name.as_bytes().to_vec(),
        data,
        method: 8,
        flags: 0,
        body: None,
        desc: None,
        zip64: false,
        local_extra: Vec::new(),
        central_extra: Vec::new(),
        comment: Vec::new(),
    }
}

impl Entry {
    fn body(&self) -> Vec<u8> {
        self.body.clone().unwrap_or_else(|| if self.method == 8 { deflate(&self.data, 6) } else { self.data.clone() })
    }
}

/// A ZIP archive written by hand, so that every field can be had any way.
#[derive(Clone)]
struct Zip {
    prefix: Vec<u8>,
    entries: Vec<Entry>,
    /// Offsets from the start of the archive rather than of the file (an SFX not adjusted).
    unadjusted: bool,
    /// The order of the central headers, by entry.
    order: Option<Vec<usize>>,
    end64: bool,
    comment: Vec<u8>,
    suffix: Vec<u8>,
}

fn archive(entries: Vec<Entry>) -> Zip {
    Zip {
        prefix: Vec::new(),
        entries,
        unadjusted: false,
        order: None,
        end64: false,
        comment: Vec::new(),
        suffix: Vec::new(),
    }
}

fn le16(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&(v as u16).to_le_bytes());
}

fn le32(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&(v as u32).to_le_bytes());
}

fn le64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_le_bytes());
}

impl Zip {
    fn bytes(&self) -> Vec<u8> {
        let mut o = self.prefix.clone();
        let shift = if self.unadjusted { self.prefix.len() as u64 } else { 0 };
        let mut at = Vec::new();
        for e in &self.entries {
            let body = e.body();
            let crc = crc32fast::hash(&e.data);
            at.push(o.len() as u64 - shift);
            let flags = e.flags | if e.desc.is_some() { 8 } else { 0 };
            o.extend_from_slice(b"PK\x03\x04");
            le16(&mut o, if e.zip64 { 45 } else { 20 });
            le16(&mut o, flags as u64);
            le16(&mut o, e.method as u64);
            le32(&mut o, 0x5021_6c4a);
            let known = e.desc.is_none();
            le32(&mut o, if known { crc as u64 } else { 0 });
            for v in [body.len() as u64, e.data.len() as u64] {
                le32(
                    &mut o,
                    if e.zip64 {
                        u32::MAX as u64
                    } else if known {
                        v
                    } else {
                        0
                    },
                );
            }
            let mut extra = e.local_extra.clone();
            if e.zip64 {
                extra.extend_from_slice(&[1, 0, 16, 0]);
                le64(&mut extra, if known { e.data.len() as u64 } else { 0 });
                le64(&mut extra, if known { body.len() as u64 } else { 0 });
            }
            le16(&mut o, e.name.len() as u64);
            le16(&mut o, extra.len() as u64);
            o.extend_from_slice(&e.name);
            o.extend_from_slice(&extra);
            o.extend_from_slice(&body);
            if let Some((sig, long)) = e.desc {
                if sig {
                    o.extend_from_slice(b"PK\x07\x08");
                }
                le32(&mut o, crc as u64);
                for v in [body.len() as u64, e.data.len() as u64] {
                    if long { le64(&mut o, v) } else { le32(&mut o, v) }
                }
            }
        }
        let cd = o.len() as u64 - shift;
        let order = self.order.clone().unwrap_or_else(|| (0..self.entries.len()).collect());
        for &i in &order {
            let e = &self.entries[i];
            let body = e.body();
            let flags = e.flags | if e.desc.is_some() { 8 } else { 0 };
            o.extend_from_slice(b"PK\x01\x02");
            le16(&mut o, 0x031e);
            le16(&mut o, if e.zip64 { 45 } else { 20 });
            le16(&mut o, flags as u64);
            le16(&mut o, e.method as u64);
            le32(&mut o, 0x5021_6c4a);
            le32(&mut o, crc32fast::hash(&e.data) as u64);
            let mut extra = e.central_extra.clone();
            if e.zip64 {
                le32(&mut o, u32::MAX as u64);
                le32(&mut o, u32::MAX as u64);
                extra.extend_from_slice(&[1, 0, 24, 0]);
                le64(&mut extra, e.data.len() as u64);
                le64(&mut extra, body.len() as u64);
                le64(&mut extra, at[i]);
            } else {
                le32(&mut o, body.len() as u64);
                le32(&mut o, e.data.len() as u64);
            }
            le16(&mut o, e.name.len() as u64);
            le16(&mut o, extra.len() as u64);
            le16(&mut o, e.comment.len() as u64);
            le16(&mut o, 0);
            le16(&mut o, 0);
            le32(&mut o, 0o100644 << 16);
            le32(&mut o, if e.zip64 { u32::MAX as u64 } else { at[i] });
            o.extend_from_slice(&e.name);
            o.extend_from_slice(&extra);
            o.extend_from_slice(&e.comment);
        }
        let size = o.len() as u64 - shift - cd;
        let n = order.len() as u64;
        if self.end64 {
            let e64 = o.len() as u64 - shift;
            o.extend_from_slice(b"PK\x06\x06");
            le64(&mut o, 44);
            le16(&mut o, 0x031e);
            le16(&mut o, 45);
            le32(&mut o, 0);
            le32(&mut o, 0);
            le64(&mut o, n);
            le64(&mut o, n);
            le64(&mut o, size);
            le64(&mut o, cd);
            o.extend_from_slice(b"PK\x06\x07");
            le32(&mut o, 0);
            le64(&mut o, e64);
            le32(&mut o, 1);
        }
        o.extend_from_slice(b"PK\x05\x06");
        le32(&mut o, 0);
        let n16 = if self.end64 { 0xffff } else { n };
        le16(&mut o, n16);
        le16(&mut o, n16);
        le32(&mut o, if self.end64 { u32::MAX as u64 } else { size });
        le32(&mut o, if self.end64 { u32::MAX as u64 } else { cd });
        le16(&mut o, self.comment.len() as u64);
        o.extend_from_slice(&self.comment);
        o.extend_from_slice(&self.suffix);
        o
    }
}

/// Entries of every kind of data: text, noise, empty, a duplicate, a directory.
fn entries() -> Vec<Entry> {
    let mut v = vec![
        entry("a.txt", text(30_000)),
        entry("noise", noise(5000)),
        entry("empty", Vec::new()),
        entry("b.txt", text(30_000)),
        entry("small", b"hello".to_vec()),
    ];
    v.push(Entry { method: 0, ..entry("dir/", Vec::new()) });
    v.push(Entry { method: 0, ..entry("stored.txt", text(4000)) });
    v
}

#[test]
fn fixtures() {
    let dir = format!("{}/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<_> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert!(names.len() >= 10);
    for name in names {
        let d = fixture(&name);
        let (f, bodies) = roundtrip_with(&d, &Options::default());
        assert!(bodies.len() >= 4 && !bodies.contains(&Body::Kept), "{name}: {bodies:?}");
        assert_eq!(count(&bodies, Body::Kept), 0, "{name}");
        // the layout is mostly predicted: well under the headers it describes
        let layout = &f.streams[2].data;
        let z = zstd::bulk::compress(layout, 3).unwrap().len();
        assert!(z < 400, "{name}: layout {} bytes, {z} compressed", layout.len());
    }
}

/// How the bodies of [`entries`] are made, written plainly.
fn plain() -> Vec<Body> {
    roundtrip(&archive(entries()).bytes())
}

#[test]
fn hand_written() {
    let d = archive(entries()).bytes();
    let b = roundtrip(&d);
    assert_eq!(b.len(), 7);
    assert_eq!(count(&b, Body::Stored), 2);
    assert_eq!(count(&b, Body::Same), 1);
    // the noise does not deflate smaller than it is kept
    assert!(count(&b, Body::Deflated) + count(&b, Body::Kept) == 4);
}

/// Every data descriptor form, zip64 fields in every header and the zip64 end records.
#[test]
fn descriptors_and_zip64() {
    for (sig, long) in [(false, false), (true, false), (false, true), (true, true)] {
        let es = entries().into_iter().map(|e| Entry { desc: Some((sig, long)), ..e }).collect();
        let d = archive(es).bytes();
        assert_eq!(roundtrip(&d), plain(), "{sig} {long}");
        // and without a central directory to tell where bodies end (a stream cut short)
        let cut = d.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        roundtrip(&d[..cut]);
    }
    let es: Vec<Entry> = entries().into_iter().map(|e| Entry { zip64: true, ..e }).collect();
    let mut z = archive(es.clone());
    z.end64 = true;
    let d = z.bytes();
    let (f, b) = roundtrip_with(&d, &Options::default());
    assert_eq!(b, plain());
    let headers = d.len() - es.iter().map(|e| e.body().len()).sum::<usize>();
    assert!(f.streams[2].data.len() < headers, "layout {} for {headers} bytes of headers", f.streams[2].data.len());
    // zip64 with data descriptors
    let es = es.into_iter().map(|e| Entry { desc: Some((true, true)), ..e }).collect();
    roundtrip(&Zip { end64: true, ..archive(es) }.bytes());
}

/// Data before the archive, offsets from either start, data after it, a comment holding
/// signatures.
#[test]
fn around_the_archive() {
    let mut z = archive(entries());
    z.prefix = b"MZ\x90\0 a self-extractor's stub PK\x03\x04 and PK\x05\x06 signatures".repeat(20);
    for unadjusted in [false, true] {
        z.unadjusted = unadjusted;
        assert_eq!(roundtrip(&z.bytes()), plain());
    }
    z.suffix = b"trailing PK\x05\x06\0\0\0\0 garbage".to_vec();
    z.comment = b"comment PK\x01\x02 PK\x05\x06".to_vec();
    roundtrip(&z.bytes());
}

/// Central headers in another order, referring to nothing, and archives one after another.
#[test]
fn directory_order() {
    let mut z = archive(entries());
    z.order = Some(vec![6, 5, 4, 3, 2, 1, 0]);
    roundtrip(&z.bytes());
    z.order = Some(vec![0, 0, 3, 3, 1]);
    roundtrip(&z.bytes());
    let mut d = z.bytes();
    // the third central header's offset points nowhere
    let p = d.windows(4).enumerate().filter(|(_, w)| *w == b"PK\x01\x02").nth(2).unwrap().0;
    d[p + 42] ^= 0x55;
    roundtrip(&d);
    let two = [archive(entries()).bytes(), archive(entries()[..3].to_vec()).bytes()].concat();
    roundtrip(&two);
}

/// Headers whose fields are wrong: they are kept as they are all the same.
#[test]
fn lying_headers() {
    let d = archive(entries()).bytes();
    let mut rng = Rng::new(7);
    let locals: Vec<usize> = d.windows(4).enumerate().filter(|(_, w)| *w == b"PK\x03\x04").map(|(i, _)| i).collect();
    for &p in &locals {
        // CRC, version, time: not sizes, which tell where bodies end
        for off in [4, 10, 14, 15] {
            let mut e = d.clone();
            e[p + off] ^= 1 + rng.below(255) as u8;
            roundtrip(&e);
        }
    }
    // a wrong compressed size: the central directory still tells
    let mut e = d.clone();
    e[locals[0] + 18] ^= 1;
    roundtrip(&e);
}

/// Bodies that are not what their method says, encrypted, of other methods, with bytes after
/// the DEFLATE stream, or cut short.
#[test]
fn odd_bodies() {
    let t = text(20_000);
    let mut deflated = deflate(&t, 9);
    let mut es = vec![
        Entry { flags: 1, ..entry("encrypted", t.clone()) },
        Entry { method: 12, body: Some(b"BZh9 not really".to_vec()), ..entry("bzip2", t.clone()) },
        Entry { method: 99, body: Some(noise(300)), ..entry("aes", t.clone()) },
        Entry { body: Some(b"not deflate at all".to_vec()), ..entry("bad", t.clone()) },
        Entry { body: Some(Vec::new()), ..entry("nothing", t.clone()) },
    ];
    let mut trailing = deflated.clone();
    trailing.extend_from_slice(b"junk");
    es.push(Entry { body: Some(trailing), ..entry("trailing", t.clone()) });
    deflated.truncate(deflated.len() / 2);
    es.push(Entry { body: Some(deflated), ..entry("cut", t.clone()) });
    let b = roundtrip(&archive(es).bytes());
    assert_eq!(b, [Body::Kept, Body::Kept, Body::Kept, Body::Kept, Body::Kept, Body::Deflated, Body::Kept]);
}

/// Entries overlapping (a "better zip bomb"): central headers refer to local headers inside
/// the first entry's body.
#[test]
fn overlapping() {
    let inner = archive(vec![entry("x", vec![0; 100_000])]).bytes();
    let first = inner.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let mut z = archive(vec![Entry { method: 0, ..entry("outer", inner[..first].to_vec()) }]);
    z.order = Some(vec![0]);
    let mut d = z.bytes();
    // a second central header pointing into the first body, at the inner local header
    let cd = d.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let mut extra = inner[first..first + 46 + 1].to_vec();
    extra[42..46].copy_from_slice(&(30 + 5u32).to_le_bytes());
    d.splice(cd..cd, extra);
    roundtrip(&d);
}

#[test]
fn truncated_and_empty() {
    let d = archive(entries()).bytes();
    for cut in [10, 40, 200, d.len() / 3, d.len() / 2, d.len() - 30, d.len() - 1] {
        if let Some(e) = formula::encode_with(&d[..cut], &Options::default()) {
            assert!(decode(&e.formula, 1).unwrap() == d[..cut]);
        }
    }
    let empty = archive(Vec::new()).bytes();
    assert_eq!(empty.len(), 22);
    assert!(formula::encode(&empty, 1).is_none());
    assert!(formula::encode(b"", 1).is_none());
    assert!(formula::encode(b"PK\x03\x04", 1).is_none());
}

/// Bodies inflate within the limit, those expanding least first; the rest are kept.
#[test]
fn inflate_limit() {
    let es = vec![entry("zeros", vec![0; 1 << 20]), entry("text", text(100_000)), entry("more", text(200_000))];
    let d = archive(es).bytes();
    let limit = |limit| roundtrip_with(&d, &Options { limit, ..Options::default() }).1;
    assert_eq!(limit(u64::MAX), [Body::Deflated; 3]);
    assert_eq!(limit(300_000), [Body::Kept, Body::Deflated, Body::Deflated]);
    assert_eq!(limit(150_000), [Body::Kept, Body::Deflated, Body::Kept]);
    assert_eq!(limit(0), [Body::Kept; 3]);
    let e = formula::encode_with(&d, &Options { limit: 300_000, ..Options::default() }).unwrap();
    assert_eq!(e.inflated, 300_000);
}

/// Bodies inflate all where the sizes their headers say fit the limit; where one holds more
/// than it says, they are measured as above.
#[test]
fn inflate_limit_by_declared_sizes() {
    let es = vec![entry("zeros", vec![0; 1 << 20]), entry("text", text(100_000)), entry("more", text(200_000))];
    let limit =
        |es: &[Entry], limit| roundtrip_with(&archive(es.to_vec()).bytes(), &Options { limit, ..Options::default() }).1;
    assert_eq!(limit(&es, 1_400_000), [Body::Deflated; 3]);
    assert_eq!(limit(&es, 1_300_000), [Body::Kept, Body::Deflated, Body::Deflated]);
    let mut lying = es.clone();
    lying[0] = Entry { body: Some(deflate(&[0; 1 << 20], 6)), ..entry("zeros", vec![0; 1000]) };
    assert_eq!(limit(&lying, 400_000), [Body::Kept, Body::Deflated, Body::Deflated]);
    lying[2] = Entry { body: Some(deflate(&text(200_000), 6)), ..entry("more", text(1000)) };
    assert_eq!(limit(&lying, 400_000), [Body::Kept, Body::Deflated, Body::Deflated]);
}

/// With a probe, a large body is rebuilt only where its data may archive smaller than it (by
/// more than its record): noise is kept, words deflated at level 1 are rebuilt, and bodies
/// below [`formula::PROBE_MIN`] are rebuilt whatever they hold.
#[test]
fn probed() {
    let mut rng = Rng::new(7);
    let vocabulary: Vec<String> = (0..2000).map(|i| format!("w{:x}", i * 2654435761u64 % 1000003)).collect();
    let mut words = Vec::new();
    while words.len() < 2_000_000 {
        words.extend_from_slice(vocabulary[rng.below(2000) as usize].as_bytes());
        words.push(b' ');
    }
    let es = vec![
        entry("noise", noise(400_000)),
        Entry { body: Some(deflate(&words, 1)), ..entry("words", words.clone()) },
        // (other noise: `noise` gives the same bytes first)
        entry("small", rng.bytes(100_000)),
    ];
    let d = archive(es).bytes();
    let probe = aqr_formula::probe::Probe::flat(aqr_formula::aqr::Level::Mid);
    let bodies = |probe| roundtrip_with(&d, &Options { probe, ..Options::default() }).1;
    assert_eq!(bodies(None), [Body::Deflated; 3]);
    assert_eq!(bodies(Some(probe)), [Body::Kept, Body::Deflated, Body::Deflated]);
    // noise held twice (a few bytes apart) gains as much as one copy costs: both are rebuilt
    let mut again = noise(400_000);
    again[1000..1100].fill(0);
    let d = archive(vec![entry("noise", noise(400_000)), entry("again", again)]).bytes();
    let bodies = roundtrip_with(&d, &Options { probe: Some(probe), ..Options::default() }).1;
    assert_eq!(bodies, [Body::Deflated; 2]);
}

/// An archive of archives like 42.zip: every level holds the same archive many times, which
/// the layout takes again rather than inflating each.
#[test]
fn nested_like_42() {
    let mut level = archive(vec![entry("0.dll", vec![0; 1 << 22])]).bytes();
    for name in ["page", "doc", "chapter"] {
        let es = (0..16).map(|i| entry(&format!("{name} {i:x}.zip"), level.clone())).collect();
        level = archive(es).bytes();
    }
    let (f, b) = roundtrip_with(&level, &Options::default());
    assert_eq!(b[0], Body::Deflated);
    assert_eq!(count(&b, Body::Same), 15);
    assert_eq!(f.streams[0].data.len(), {
        let z = aqr_zip::zip::records(&level).2;
        let aqr_zip::zip::Rec::Local(l) = &z[0] else { panic!() };
        aqr_zip::zip::deflate_len(&level[l.body.clone()]).unwrap().1
    });
}

/// A damaged layout is an error, never a panic.
#[test]
fn damaged_inputs() {
    let d = archive(entries()).bytes();
    let f = formula::encode(&d, 1).unwrap();
    let mut rng = Rng::new(3);
    for i in 0..2000 {
        let mut g = f.clone();
        let s = &mut g.streams[[2, 2, 2, 0, 1, 3][i % 6]].data;
        if s.is_empty() {
            continue;
        }
        match i % 3 {
            0 => {
                let p = rng.below(s.len() as u64) as usize;
                s[p] ^= 1 << rng.below(8);
            }
            1 => s.truncate(rng.below(s.len() as u64) as usize),
            _ => s.insert(rng.below(s.len() as u64) as usize, rng.below(256) as u8),
        }
        let _ = decode(&g, 1);
    }
}

/// Archives damaged anywhere are rebuilt exactly all the same, as records and gaps.
#[test]
fn damaged_archives() {
    let mut rng = Rng::new(11);
    for name in ["py-stream.zip", "infozip.zip", "go.zip", "bsdtar.zip", "ditto.zip", "py-zip64.zip"] {
        let d = fixture(name);
        for i in 0..150 {
            let mut e = d.clone();
            for _ in 0..1 + i % 4 {
                let p = rng.below(e.len() as u64) as usize;
                match rng.below(4) {
                    0 => e[p] ^= 1 << rng.below(8),
                    1 => e[p] = rng.below(256) as u8,
                    2 => e.insert(p, rng.below(256) as u8),
                    _ => e.truncate(p.max(1)),
                }
            }
            if let Some(f) = formula::encode_with(&e, &Options::default()) {
                assert!(decode(&f.formula, 2).unwrap() == e, "{name} #{i}");
            }
        }
    }
}

/// The contents of the entries are a stream for each kind their names give, sliced in order,
/// the unknown ones in `raw`; the inputs stay the first four streams.
#[test]
fn contents_by_kind() {
    use aqr_formula::Kind;
    let kind = |name: &[u8]| match name.rsplit(|&b| b == b'.').next() {
        Some(b"txt") => Kind::Text,
        Some(b"png") => Kind::Format("png"),
        _ => Kind::Unknown,
    };
    let mut es = entries();
    es.push(entry("image.png", text(2000)));
    es.push(Entry { method: 0, ..entry("tail.png", noise(3000)) });
    let d = archive(es).bytes();
    let opt = Options { kind, ..Options::default() };
    let f = formula::encode_with(&d, &opt).unwrap().formula;
    let classes: Vec<_> = f.streams.iter().map(|s| (&*s.class, s.kind)).collect();
    assert_eq!(
        classes,
        [
            ("raw", Kind::Unknown),
            ("flate/record", Kind::Unknown),
            ("zip/layout", Kind::Unknown),
            ("zip/kept", Kind::Unknown),
            ("zip/text", Kind::Text),
            ("zip/png", Kind::Format("png")),
        ]
    );
    let text_len = |n| text(n).len();
    // (b.txt is a.txt again, taken as the same body)
    assert_eq!(f.streams[4].data.len(), text_len(30_000) + text_len(4000));
    assert_eq!(f.streams[5].data.len(), text_len(2000) + 3000);
    // the same archive, and the same data as without kinds
    let Node::Apply(a) = &f.root else { panic!() };
    let module = |a: &aqr_formula::Apply, ins: &[&[u8]]| {
        formula::apply(a, ins, 1).map_err(|e| aqr_formula::Error::new(format!("{e:?}")))
    };
    assert!(f.eval(&module).unwrap() == d);
    let plain = formula::encode_with(&d, &Options::default()).unwrap().formula;
    let data = |f: &Formula, n: &Node| {
        let mut g = f.clone();
        g.root = n.clone();
        g.eval(&module).unwrap()
    };
    let Node::Apply(p) = &plain.root else { panic!() };
    assert!(data(&f, &a.inputs[0]) == data(&plain, &p.inputs[0]));
    assert!(plain.streams.len() == 4 && f.streams[1..4] == plain.streams[1..4]);
}
