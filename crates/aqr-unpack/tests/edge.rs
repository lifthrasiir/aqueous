//! Edge cases of the readers: archives lying about sizes, headers disagreeing, names that
//! collide, directories cut. An error or a note, never a panic, a hang or an allocation the
//! data cannot hold (the allocator refuses those: the case is named on standard error).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use aqr_common::crc32;
use aqr_unpack::{Done, EntryNode, Format, Opened, Sink, Tree};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Rec {
    contents: Mutex<BTreeMap<usize, Vec<u8>>>,
    notes: Mutex<Vec<String>>,
}

impl Sink for Rec {
    fn entries(&self, _: &Tree, _: &[usize]) -> Result<(), String> {
        Ok(())
    }
    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        self.contents.lock().unwrap().insert(c, d);
        Ok(())
    }
    fn note(&self, what: &str) {
        self.notes.lock().unwrap().push(what.to_string());
    }
}

/// An archive unpacked: the files by path with their contents, the notes, and what `Done` says.
struct Out {
    done: Done,
    files: BTreeMap<String, Vec<u8>>,
    notes: Vec<String>,
}

fn unpack(f: Format, d: &[u8]) -> Result<Out, String> {
    let r = Rec::default();
    let done = Opened::open(f, d)?.unpack(2, &r)?;
    let contents = r.contents.into_inner().unwrap();
    let mut paths: Vec<String> = Vec::new();
    let mut files = BTreeMap::new();
    for e in &done.tree.entries {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let path = e.parent.map_or(name.clone(), |p| format!("{}/{name}", paths[p]));
        if let EntryNode::File(c) = e.node {
            files.insert(path.clone(), contents.get(&c).cloned().unwrap_or_default());
        }
        paths.push(path);
    }
    Ok(Out { done, files, notes: r.notes.into_inner().unwrap() })
}

/// `unpack` as a case: named for the allocator, within a time, a panic a failure of the test.
fn case(name: &str, f: Format, d: &[u8]) -> Result<Out, String> {
    aqr_testkit::within(LIMIT, || {
        aqr_testkit::case(name);
        let r = unpack(f, d);
        // (what the data holds is a few megabytes at most here: a size told must not be reserved)
        assert!(aqr_testkit::most() < 256 << 20, "an allocation of {} bytes", aqr_testkit::most());
        r
    })
    .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn text() -> Vec<u8> {
    (0..1000).flat_map(|i| format!("line {i} of some text\n").into_bytes()).collect()
}

// ---- ZIP

/// An entry of [`zip_of`].
#[derive(Clone)]
struct E {
    name: Vec<u8>,
    method: u16,
    flags: u16,
    /// The data as stored (compressed).
    body: Vec<u8>,
    crc: u32,
    /// The sizes the central directory tells.
    size: u64,
    csize: u64,
    /// Sizes and offset in the zip64 field (the 32-bit ones all ones), the local header's too.
    zip64: bool,
    /// What the local header tells instead: CRC-32, compressed and uncompressed sizes.
    local: Option<(u32, u32, u32)>,
    /// The name the local header has, where it is another.
    local_name: Option<Vec<u8>>,
    /// The central directory's offset of the local header, where it is another.
    offset: Option<u64>,
}

impl E {
    fn stored(name: &str, data: &[u8]) -> E {
        E {
            name: name.as_bytes().to_vec(),
            method: 0,
            flags: 0,
            body: data.to_vec(),
            crc: crc32::hash(data),
            size: data.len() as u64,
            csize: data.len() as u64,
            zip64: false,
            local: None,
            local_name: None,
            offset: None,
        }
    }

    /// Of `method`, `body` as stored of data of `size` bytes and CRC-32 `crc`.
    fn packed(name: &str, method: u16, body: &[u8], size: u64, crc: u32) -> E {
        E { method, csize: body.len() as u64, body: body.to_vec(), size, crc, ..E::stored(name, &[]) }
    }
}

fn le(v: &mut Vec<u8>, x: u64, n: usize) {
    v.extend_from_slice(&x.to_le_bytes()[..n]);
}

/// A ZIP archive of `entries` (in order), its end zip64 where `end64`.
fn zip_of(entries: &[E], end64: bool) -> Vec<u8> {
    const M: u64 = u32::MAX as u64;
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for e in entries {
        let at = out.len() as u64;
        let (lcrc, lc, lu) = match (e.local, e.zip64) {
            (Some(l), _) => l,
            (None, true) => (e.crc, M as u32, M as u32),
            (None, false) => (e.crc, e.csize as u32, e.size as u32),
        };
        let lname = e.local_name.as_ref().unwrap_or(&e.name);
        out.extend_from_slice(b"PK\x03\x04");
        le(&mut out, 45, 2);
        le(&mut out, e.flags as u64, 2);
        le(&mut out, e.method as u64, 2);
        out.extend_from_slice(&[0, 0, 0x21, 0]);
        le(&mut out, lcrc as u64, 4);
        le(&mut out, lc as u64, 4);
        le(&mut out, lu as u64, 4);
        le(&mut out, lname.len() as u64, 2);
        le(&mut out, if e.zip64 { 20 } else { 0 }, 2);
        out.extend_from_slice(lname);
        if e.zip64 {
            le(&mut out, 1, 2);
            le(&mut out, 16, 2);
            le(&mut out, e.size, 8);
            le(&mut out, e.csize, 8);
        }
        out.extend_from_slice(&e.body);
        // the central header
        let off = e.offset.unwrap_or(at);
        let mut x = Vec::new();
        let (su, sc, so) = if e.zip64 {
            le(&mut x, 1, 2);
            le(&mut x, 24, 2);
            le(&mut x, e.size, 8);
            le(&mut x, e.csize, 8);
            le(&mut x, off, 8);
            (M, M, M)
        } else {
            (e.size, e.csize, off)
        };
        cd.extend_from_slice(b"PK\x01\x02");
        le(&mut cd, 45, 2);
        le(&mut cd, 45, 2);
        le(&mut cd, e.flags as u64, 2);
        le(&mut cd, e.method as u64, 2);
        cd.extend_from_slice(&[0, 0, 0x21, 0]);
        le(&mut cd, e.crc as u64, 4);
        le(&mut cd, sc, 4);
        le(&mut cd, su, 4);
        le(&mut cd, e.name.len() as u64, 2);
        le(&mut cd, x.len() as u64, 2);
        cd.extend_from_slice(&[0; 10]);
        le(&mut cd, so, 4);
        cd.extend_from_slice(&e.name);
        cd.extend_from_slice(&x);
    }
    let (start, n) = (out.len() as u64, entries.len() as u64);
    out.extend_from_slice(&cd);
    if end64 {
        let at = out.len() as u64;
        out.extend_from_slice(b"PK\x06\x06");
        le(&mut out, 44, 8);
        le(&mut out, 45, 2);
        le(&mut out, 45, 2);
        le(&mut out, 0, 8);
        le(&mut out, n, 8);
        le(&mut out, n, 8);
        le(&mut out, cd.len() as u64, 8);
        le(&mut out, start, 8);
        out.extend_from_slice(b"PK\x06\x07");
        le(&mut out, 0, 4);
        le(&mut out, at, 8);
        le(&mut out, 1, 4);
    }
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    let (n16, size, start) = if end64 { (0xffff, M, M) } else { (n, cd.len() as u64, start) };
    le(&mut out, n16, 2);
    le(&mut out, n16, 2);
    le(&mut out, size, 4);
    le(&mut out, start, 4);
    le(&mut out, 0, 2);
    out
}

/// A DEFLATE stream (fixed Huffman codes) of `n` bytes of zeros: a literal, then matches of 258.
fn deflate_zeros(n: usize) -> Vec<u8> {
    struct Bits(Vec<u8>, u32, u32);
    impl Bits {
        /// `n` bits of `v`, least significant first.
        fn put(&mut self, v: u32, n: u32) {
            for i in 0..n {
                self.1 |= ((v >> i) & 1) << self.2;
                self.2 += 1;
                if self.2 == 8 {
                    self.0.push(self.1 as u8);
                    (self.1, self.2) = (0, 0);
                }
            }
        }
        /// A Huffman code of `n` bits, most significant first.
        fn code(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                self.put((v >> i) & 1, 1);
            }
        }
    }
    let mut b = Bits(Vec::new(), 0, 0);
    b.put(1, 1);
    b.put(1, 2);
    // literal 0: code 0x30, 8 bits
    b.code(0x30, 8);
    let mut left = n - 1;
    while left > 0 {
        let len = left.min(258);
        if len < 3 {
            for _ in 0..len {
                b.code(0x30, 8);
            }
            break;
        }
        if len == 258 {
            // symbol 285: 8 bits, 0xc0 + (285 - 280)
            b.code(0xc0 + 5, 8);
        } else {
            // lengths 3..=10 are symbols 257..=264 (7 bits from 0): not used by the tail otherwise
            let sym = match len {
                3..=10 => 257 + (len - 3),
                _ => unreachable!("n of a multiple of 258 and 1"),
            };
            b.code((sym - 256) as u32, 7);
        }
        // distance 1: symbol 0, 5 bits
        b.code(0, 5);
        left -= len;
    }
    b.code(0, 7);
    if b.2 > 0 {
        b.0.push(b.1 as u8);
    }
    b.0
}

/// The entries of tree.zip as bodies: stored, DEFLATE, bzip2, LZMA, of the text.
fn bodies() -> Vec<E> {
    use aqr_zip::zip as z;
    let d = data("tree.zip");
    let (base, cd) = z::find_end(&d).unwrap();
    let mut out = Vec::new();
    let mut p = cd.start;
    while p < cd.end {
        let c = z::central_at(&d, p).unwrap();
        p = c.comment.end;
        let (off, info) = c.info(&d);
        let l = (off as i64 + base) as usize;
        let u16_at = |at: usize| u16::from_le_bytes([d[at], d[at + 1]]) as usize;
        let start = l + 30 + u16_at(l + 26) + u16_at(l + 28);
        let method = u16_at(c.name.start - 46 + 10) as u16;
        let name = String::from_utf8_lossy(&d[c.name.clone()]).into_owned();
        if info.usize == text().len() as u64 {
            out.push(E::packed(&name, method, &d[start..start + info.csize as usize], info.usize, info.crc));
        }
    }
    out
}

#[test]
fn rebuilt_fixture() {
    let b = bodies();
    assert_eq!(b.iter().map(|e| e.method).collect::<Vec<_>>(), [0, 8, 12, 14]);
    for end64 in [false, true] {
        let o = case("rebuilt", Format::Zip, &zip_of(&b, end64)).unwrap();
        assert_eq!(o.files.len(), 4);
        assert!(o.files.values().all(|v| *v == text()));
    }
}

/// ZIP64: the sizes and the offset in the zip64 field (entries' and end's), the end record's
/// fields all ones.
#[test]
fn zip64() {
    let mut es = bodies();
    for e in &mut es {
        e.zip64 = true;
    }
    es.push({
        let mut e = E::stored("empty", b"");
        e.zip64 = true;
        e
    });
    let o = case("zip64", Format::Zip, &zip_of(&es, true)).unwrap();
    assert_eq!(o.files.len(), 5);
    assert!(o.files.iter().all(|(k, v)| *v == if k == "empty" { Vec::new() } else { text() }));
    // the entries alone zip64, the end not; and the end alone
    let o = case("zip64 entries", Format::Zip, &zip_of(&es, false)).unwrap();
    assert_eq!(o.files.len(), 5);
    let o = case("zip64 end", Format::Zip, &zip_of(&bodies(), true)).unwrap();
    assert_eq!(o.files.len(), 4);
}

/// A zip64 field too short for the sizes the 32-bit fields refer it for: the sizes stay all ones
/// (4 GiB of data that is not there): an error.
#[test]
fn zip64_field_short() {
    let mut es = bodies();
    es.truncate(1);
    let mut z = zip_of(&es, false);
    // the central header's sizes made all ones without a field
    let at = z.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    z[at + 20..at + 28].fill(0xff);
    assert!(case("zip64 field missing", Format::Zip, &z).is_err());
}

/// Entries that say they hold more than any DEFLATE, bzip2, LZMA, Zstandard or xz stream could:
/// the data is not there. An error, with no room of the size told reserved.
#[test]
fn lying_sizes() {
    let zstd = |n: usize| zstd::encode_all(&vec![0u8; n][..], 3).unwrap();
    let mut es = bodies();
    es.push(E::packed("zstd", 93, &zstd(1000), 1000, crc32::hash(&[0u8; 1000])));
    es.push(E::packed("xz", 95, &data("one.txt.xz"), 1, 0));
    for e in &mut es {
        for size in [u32::MAX as u64 - 1, 1 << 32, 1 << 40, (1 << 62) + 5, u64::MAX] {
            let mut lying = e.clone();
            lying.size = size;
            lying.zip64 = size >= u32::MAX as u64;
            // (a lying size alone, and with the CRC of the real data)
            let name = format!("method {} declared {size}", e.method);
            let r = case(&name, Format::Zip, &zip_of(&[lying], false));
            assert!(r.is_err(), "{name}: {:?}", r.map(|o| o.files.len()));
        }
    }
}

/// The units' sizes are summed for the progress: sizes past 2^64 together are no panic.
#[test]
fn sizes_sum() {
    let mut a = E::stored("a", b"x");
    a.size = 1 << 63;
    a.zip64 = true;
    let mut b = a.clone();
    b.name = b"b".to_vec();
    let r = case("sizes sum", Format::Zip, &zip_of(&[a, b], false));
    assert!(r.is_err());
}

/// An honest entry that is highly compressed: 64 MB of zeros by DEFLATE (1032 to 1) and by
/// Zstandard.
#[test]
fn highly_compressed() {
    let n = 1 + 258 * 260_000;
    let zeros = vec![0u8; n];
    let crc = crc32::hash(&zeros);
    let es = [
        E::packed("deflate", 8, &deflate_zeros(n), n as u64, crc),
        E::packed("zstd", 93, &zstd::encode_all(&zeros[..], 3).unwrap(), n as u64, crc),
    ];
    let o = case("highly compressed", Format::Zip, &zip_of(&es, false)).unwrap();
    assert_eq!(o.files.len(), 2);
    assert!(o.files.values().all(|v| v.len() == n && v.iter().all(|&b| b == 0)));
}

/// The local header says other things than the central directory: the central directory
/// rules (names, sizes, CRC); a data descriptor's zero sizes there are no matter.
#[test]
fn local_and_central_disagree() {
    let t = text();
    let mut a = E::stored("a.txt", &t);
    a.local = Some((0, 0, 0));
    a.flags = 8;
    let mut b = E::stored("b.txt", &t);
    b.local_name = Some(b"other.txt".to_vec());
    let mut c = E::stored("c.txt", &t);
    c.local = Some((1, 7, 9));
    let o = case("disagree", Format::Zip, &zip_of(&[a, b, c], false)).unwrap();
    assert_eq!(o.files.keys().collect::<Vec<_>>(), ["a.txt", "b.txt", "c.txt"]);
    assert!(o.files.values().all(|v| *v == t));
}

/// An offset of the central directory that is no local header: past the end, before the
/// start, in the middle of data, all ones.
#[test]
fn central_offsets() {
    let t = text();
    for off in [1u64, 5000, 1 << 20, u32::MAX as u64 - 1, 1 << 40, u64::MAX - 1, u64::MAX] {
        let mut e = E::stored("a.txt", &t);
        e.offset = Some(off);
        e.zip64 = off >= u32::MAX as u64;
        let r = case(&format!("offset {off}"), Format::Zip, &zip_of(&[e], false));
        assert!(r.is_err(), "offset {off}");
    }
    // a local header whose name and extra fields run past the end
    let mut z = zip_of(&[E::stored("a.txt", &t)], false);
    z[28..30].copy_from_slice(&[0xff, 0xff]);
    assert!(case("local extra past the end", Format::Zip, &z).is_err());
    z[26..28].copy_from_slice(&[0xff, 0xff]);
    assert!(case("local name past the end", Format::Zip, &z).is_err());
}

/// The same path twice (ZIP): the later one kept as tar's, no panic; with a directory and a
/// file of one name, and names holding a zero byte (left out, told).
#[test]
fn zip_names() {
    let es = [E::stored("a", b"first"), E::stored("a", b"second"), E::stored("b/", b""), E::stored("b", b"file")];
    let o = case("duplicates", Format::Zip, &zip_of(&es, false)).unwrap();
    assert!(o.files.get("a").is_some_and(|v| v == b"second" || v == b"first"), "{:?}", o.files);
    assert!(o.done.tree.entries.len() <= 3, "{} entries", o.done.tree.entries.len());
    let es = [E::stored("a\0b", b"x"), E::stored("c/\0", b"y"), E::stored("ok", b"z")];
    let o = case("nul", Format::Zip, &zip_of(&es, false)).unwrap();
    assert_eq!(o.files.keys().collect::<Vec<_>>(), ["ok"]);
    assert_eq!(o.done.skipped, 2, "{:?}", o.notes);
}

/// The end of central directory found among what looks like it: its signature in the archive's
/// comment, data before the archive (offsets relative to its start), and a directory cut.
#[test]
fn find_end() {
    let t = text();
    let es = [E::stored("a.txt", &t)];
    let z = zip_of(&es, false);
    // a comment holding an end record of its own (a directory that is none)
    let mut c = z.clone();
    let n = c.len();
    c[n - 2..].copy_from_slice(&40u16.to_le_bytes());
    let mut fake = b"PK\x05\x06\0\0\0\0\x01\0\x01\0".to_vec();
    fake.extend_from_slice(&[0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    fake.resize(40, 0);
    c.extend_from_slice(&fake);
    let o = case("signature in the comment", Format::Zip, &c).unwrap();
    assert_eq!(o.files.len(), 1);
    // the comment is the signature alone
    let mut c = z.clone();
    let n = c.len();
    c[n - 2..].copy_from_slice(&4u16.to_le_bytes());
    c.extend_from_slice(b"PK\x05\x06");
    assert!(case("signature alone in the comment", Format::Zip, &c).is_ok());
    // a prefix (a stub before the archive): the offsets are of the archive's start
    let mut p = b"MZ stub ".repeat(100);
    p.extend_from_slice(&z);
    let o = case("prefix", Format::Zip, &p).unwrap();
    assert_eq!(o.files["a.txt"], t);
    // the directory cut: the end tells more than there is before it
    let at = z.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let mut cut = z.clone();
    cut[at + 12..at + 16].copy_from_slice(&(1u32 << 30).to_le_bytes());
    assert!(case("directory size past the start", Format::Zip, &cut).is_err());
    let mut cut = z.clone();
    cut[at + 16..at + 20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(case("directory offset past the end", Format::Zip, &cut).is_err());
    // the directory's last header cut
    let cd = z.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let mut short = z[..cd + 30].to_vec();
    short.extend_from_slice(&z[at..]);
    assert!(case("cut central header", Format::Zip, &short).is_err());
    // no end record at all, and nothing
    assert!(case("end cut", Format::Zip, &z[..z.len() - 10]).is_err());
    assert!(case("empty", Format::Zip, &[]).is_err());
}
