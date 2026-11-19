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

// ---- tar

/// A ustar header of `name` (of kind `kind`, size field `size`, link `link`).
fn tar_header(name: &[u8], kind: u8, size: u64, link: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[..name.len()].copy_from_slice(name);
    b[100..108].copy_from_slice(b"0000644\0");
    b[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    b[136..148].copy_from_slice(b"00000000000\0");
    b[156] = kind;
    b[157..157 + link.len()].copy_from_slice(link);
    b[257..265].copy_from_slice(b"ustar\x0000");
    tar_sum(&mut b);
    b
}

fn tar_sum(b: &mut [u8]) {
    b[148..156].fill(b' ');
    let sum: u32 = b.iter().map(|&x| x as u32).sum();
    b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

/// A member: its header and body (padded).
fn tar_member(name: &[u8], kind: u8, body: &[u8], link: &[u8]) -> Vec<u8> {
    let mut v = tar_header(name, kind, body.len() as u64, link);
    v.extend_from_slice(body);
    v.resize(v.len().next_multiple_of(512), 0);
    v
}

fn tar_end(mut v: Vec<u8>) -> Vec<u8> {
    v.extend_from_slice(&[0; 1024]);
    v
}

fn pax_record(k: &str, v: &[u8]) -> Vec<u8> {
    let rest = k.len() + v.len() + 3;
    let mut len = rest + 1;
    while len.to_string().len() + rest != len {
        len += 1;
    }
    let mut r = format!("{len} {k}=").into_bytes();
    r.extend_from_slice(v);
    r.push(b'\n');
    r
}

fn tar_files(d: &[u8]) -> Out {
    case("tar", Format::Tar(None), d).unwrap()
}

/// A path given again takes the place of the one before (a file or a link for either); a
/// directory and a file of one name leave the later out, told.
#[test]
fn tar_duplicates() {
    let mut t = Vec::new();
    t.extend(tar_member(b"a", b'0', b"first", b""));
    t.extend(tar_member(b"a", b'0', b"second", b""));
    t.extend(tar_member(b"d/", b'5', b"", b""));
    t.extend(tar_member(b"d", b'0', b"file after dir", b""));
    t.extend(tar_member(b"f", b'0', b"file", b""));
    t.extend(tar_member(b"f/", b'5', b"", b""));
    t.extend(tar_member(b"f/in", b'0', b"in", b""));
    t.extend(tar_member(b"l", b'2', b"", b"a"));
    t.extend(tar_member(b"l", b'0', b"link then file", b""));
    let o = tar_files(&tar_end(t));
    assert_eq!(o.files["a"], b"second");
    // (a directory and a file of one name: the later left out, told, and what is under it too)
    assert_eq!(o.files["f"], b"file");
    assert!(!o.files.contains_key("d") && !o.files.contains_key("f/in"));
    assert_eq!(o.files["l"], b"link then file");
    assert_eq!(o.done.skipped, 3, "{:?}", o.notes);
}

/// A zero byte in a name: the header's field ends there (as C strings do); pax's `path` and
/// GNU's long names with one are left out, told.
#[test]
fn tar_nul_names() {
    let mut t = Vec::new();
    t.extend(tar_member(b"cut\0off", b'0', b"x", b""));
    let mut p = pax_record("path", b"pax\0name");
    p.extend(pax_record("mtime", b"1.5"));
    t.extend(tar_member(b"PaxHeader", b'x', &p, b""));
    t.extend(tar_member(b"plain", b'0', b"y", b""));
    t.extend(tar_member(b"././@LongLink", b'L', b"long\0name\0", b""));
    t.extend(tar_member(b"short", b'0', b"z", b""));
    t.extend(tar_member(b"ok", b'0', b"w", b""));
    let o = tar_files(&tar_end(t));
    assert!(o.files.contains_key("cut"), "{:?}", o.files.keys());
    assert!(o.files.contains_key("ok"));
    assert!(!o.files.keys().any(|k| k.contains('\0')));
    // (the pax path's NUL: left out; the long name's ends at its NUL, as C strings do)
    assert!(o.notes.iter().any(|n| n.contains("reaching out")), "{:?}", o.notes);
}

/// Sparse members (old GNU's with a map of extension headers, pax's `GNU.sparse` records),
/// multi-volume parts and members of types not known: told, and what follows still read.
#[test]
fn tar_sparse_and_volumes() {
    let mut t = Vec::new();
    // an old GNU sparse header with one extension block (isextended = byte 482)
    let mut s = tar_header(b"sparse", b'S', 512, b"");
    s[482] = 1;
    tar_sum(&mut s);
    t.extend(s);
    t.extend(vec![0u8; 512]);
    t.extend(vec![7u8; 512]);
    t.extend(tar_member(b"after1", b'0', b"1", b""));
    // an old GNU sparse header with no extension
    let mut s = tar_header(b"sparse2", b'S', 3, b"");
    s[482] = 0;
    tar_sum(&mut s);
    t.extend(s);
    t.extend(tar_member(b"", b'0', b"abc", b"")[512..].to_vec());
    t.extend(tar_member(b"after2", b'0', b"2", b""));
    // pax's
    let mut p = pax_record("GNU.sparse.major", b"1");
    p.extend(pax_record("GNU.sparse.name", b"sp3"));
    t.extend(tar_member(b"PaxHeader", b'x', &p, b""));
    t.extend(tar_member(b"GNUSparseFile.0/sp3", b'0', b"data", b""));
    t.extend(tar_member(b"after3", b'0', b"3", b""));
    // multi-volume
    t.extend(tar_member(b"mv", b'M', b"rest of a file", b""));
    t.extend(tar_member(b"after4", b'0', b"4", b""));
    t.extend(tar_member(b"typed", b'Z', b"taken for a file", b""));
    let o = tar_files(&tar_end(t));
    let names: Vec<_> = o.files.keys().map(String::as_str).collect();
    assert_eq!(names, ["after1", "after2", "after3", "after4", "typed"], "{:?}", o.notes);
    assert!(o.done.skipped >= 4, "{:?}", o.notes);
}

/// Hard links to a missing target, to one that comes later, to a directory, to themselves.
#[test]
fn tar_hard_links() {
    let mut t = Vec::new();
    t.extend(tar_member(b"missing", b'1', b"", b"nowhere"));
    t.extend(tar_member(b"early", b'1', b"", b"later"));
    t.extend(tar_member(b"later", b'0', b"data", b""));
    t.extend(tar_member(b"dir/", b'5', b"", b""));
    t.extend(tar_member(b"todir", b'1', b"", b"dir"));
    t.extend(tar_member(b"self", b'1', b"", b"self"));
    t.extend(tar_member(b"out", b'1', b"", b"../x"));
    t.extend(tar_member(b"empty", b'1', b"", b""));
    t.extend(tar_member(b"good", b'1', b"", b"later"));
    let o = tar_files(&tar_end(t));
    assert_eq!(o.files["later"], b"data");
    assert_eq!(o.files["good"], b"data");
    for gone in ["missing", "early", "todir", "self", "out", "empty"] {
        assert!(!o.files.contains_key(gone), "{gone}");
    }
    assert_eq!(o.done.skipped, 6, "{:?}", o.notes);
}

/// A size past the data (the header's, base-256's, pax's): an error, with no room of that size
/// reserved; sizes near 2^64 as the skipping and the padding take them.
#[test]
fn tar_sizes_past_data() {
    for size in [1u64 << 20, 1 << 40, (1 << 63) + 1, u64::MAX - 1, u64::MAX - 511] {
        for kind in *b"052SxLM" {
            let mut h = tar_header(b"a", kind, 0, b"t");
            h[124] = 0x80;
            h[125..128].fill(0);
            h[128..136].copy_from_slice(&size.to_be_bytes());
            tar_sum(&mut h);
            let mut t = h;
            t.extend(vec![b'x'; 1000]);
            let name = format!("size {size} kind {}", kind as char);
            let r = case(&name, Format::Tar(None), &tar_end(t));
            assert!(r.is_err(), "{name}");
        }
        // pax's size
        let mut t = tar_member(b"PaxHeader", b'x', &pax_record("size", size.to_string().as_bytes()), b"");
        t.extend(tar_member(b"a", b'0', b"short", b""));
        let r = case(&format!("pax size {size}"), Format::Tar(None), &tar_end(t));
        assert!(r.is_err(), "pax size {size}");
    }
}

/// Fields at their extremes: times, modes, ids, devices.
#[test]
fn tar_fields_extreme() {
    let mut h = tar_header(b"a", b'0', 1, b"");
    h[100..108].fill(0xff);
    h[108..124].fill(0x80);
    h[136..148].fill(0xff);
    h[136] = 0x80;
    h[329..345].fill(0xff);
    tar_sum(&mut h);
    let mut t = h;
    t.extend(vec![b'x'; 512]);
    let mut p = pax_record("mtime", b"-9223372036854775808.999999999");
    p.extend(pax_record("atime", b"9223372036854775807.5"));
    p.extend(pax_record("uid", b"99999999999999999999999"));
    p.extend(pax_record("SCHILY.devmajor", b"-1"));
    t.extend(tar_member(b"PaxHeader", b'x', &p, b""));
    t.extend(tar_member(b"b", b'0', b"y", b""));
    // (no panic, in whichever way it ends)
    let _ = case("extreme fields", Format::Tar(None), &tar_end(t));
}

// ---- 7z

const COPY: &[u8] = &[0];
const LZMA: &[u8] = &[3, 1, 1];
const LZMA2: &[u8] = &[0x21];
const DEFLATE: &[u8] = &[4, 1, 8];
const BZIP2: &[u8] = &[4, 2, 2];

/// A folder of one coder: its id and properties, the packed stream, the size it tells of its
/// output, its files' sizes and CRC-32s (a size each, the last one's left to the folder's).
struct F {
    id: Vec<u8>,
    props: Vec<u8>,
    pack: Vec<u8>,
    size: u64,
    subs: Vec<(u64, u32)>,
}

impl F {
    /// Of the data `d` as stored (the packed stream is `d`), a file.
    fn copy(d: &[u8]) -> F {
        F::of(COPY, &[], d, d.len() as u64, d)
    }

    /// Coder `id` of `props` over `pack`, giving `size` bytes, the file of data `d`.
    fn of(id: &[u8], props: &[u8], pack: &[u8], size: u64, d: &[u8]) -> F {
        F {
            id: id.to_vec(),
            props: props.to_vec(),
            pack: pack.to_vec(),
            size,
            subs: vec![(d.len() as u64, crc32::hash(d))],
        }
    }
}

/// A file of the 7z archive's list.
#[derive(Clone)]
struct Fl {
    name: String,
    stream: bool,
    /// Of no stream: a file (else a directory).
    empty_file: bool,
    anti: bool,
}

impl Fl {
    fn data(name: &str) -> Fl {
        Fl { name: name.into(), stream: true, empty_file: false, anti: false }
    }
    fn empty(name: &str) -> Fl {
        Fl { name: name.into(), stream: false, empty_file: true, anti: false }
    }
    fn dir(name: &str) -> Fl {
        Fl { name: name.into(), stream: false, empty_file: false, anti: false }
    }
    fn anti(mut self) -> Fl {
        self.anti = true;
        self
    }
}

fn number(out: &mut Vec<u8>, v: u64) {
    let n = (0..8).find(|&n| v < 1u64 << (7 * (n + 1))).unwrap_or(8);
    let first = if n == 8 { 0xff } else { (0xff00u16 >> n) as u8 | (v >> (8 * n)) as u8 };
    out.push(first);
    out.extend(&v.to_le_bytes()[..n]);
}

fn bits(out: &mut Vec<u8>, b: &[bool]) {
    let mut v = vec![0u8; b.len().div_ceil(8)];
    b.iter().enumerate().filter(|x| *x.1).for_each(|(i, _)| v[i / 8] |= 0x80 >> (i % 8));
    out.extend(v);
}

/// A 7z archive of `folders` (a plain header) and the files `files`.
fn sevenz_of(folders: &[F], files: &[Fl]) -> Vec<u8> {
    let mut h = vec![1];
    if !folders.is_empty() {
        h.push(4);
        h.push(6);
        number(&mut h, 0);
        number(&mut h, folders.len() as u64);
        h.push(9);
        folders.iter().for_each(|f| number(&mut h, f.pack.len() as u64));
        h.push(0);
        h.push(7);
        h.push(11);
        number(&mut h, folders.len() as u64);
        h.push(0);
        for f in folders {
            number(&mut h, 1);
            h.push(f.id.len() as u8 | if f.props.is_empty() { 0 } else { 0x20 });
            h.extend(&f.id);
            if !f.props.is_empty() {
                number(&mut h, f.props.len() as u64);
                h.extend(&f.props);
            }
        }
        h.push(12);
        folders.iter().for_each(|f| number(&mut h, f.size));
        h.push(0);
        h.push(8);
        h.push(13);
        folders.iter().for_each(|f| number(&mut h, f.subs.len() as u64));
        h.push(9);
        for f in folders {
            f.subs.iter().take(f.subs.len().saturating_sub(1)).for_each(|s| number(&mut h, s.0));
        }
        h.push(10);
        h.push(1);
        folders.iter().flat_map(|f| &f.subs).for_each(|s| h.extend(s.1.to_le_bytes()));
        h.extend([0, 0]);
    }
    h.push(5);
    number(&mut h, files.len() as u64);
    let empties: Vec<&Fl> = files.iter().filter(|f| !f.stream).collect();
    if !empties.is_empty() {
        let mut b = Vec::new();
        bits(&mut b, &files.iter().map(|f| !f.stream).collect::<Vec<_>>());
        h.push(14);
        number(&mut h, b.len() as u64);
        h.extend(b);
        let mut b = Vec::new();
        bits(&mut b, &empties.iter().map(|f| f.empty_file).collect::<Vec<_>>());
        h.push(15);
        number(&mut h, b.len() as u64);
        h.extend(b);
        if empties.iter().any(|f| f.anti) {
            let mut b = Vec::new();
            bits(&mut b, &empties.iter().map(|f| f.anti).collect::<Vec<_>>());
            h.push(16);
            number(&mut h, b.len() as u64);
            h.extend(b);
        }
    }
    let mut names = vec![0];
    for f in files {
        f.name.encode_utf16().chain([0]).for_each(|u| names.extend(u.to_le_bytes()));
    }
    h.push(17);
    number(&mut h, names.len() as u64);
    h.extend(names);
    h.extend([0, 0]);
    let packed: Vec<u8> = folders.iter().flat_map(|f| f.pack.iter().copied()).collect();
    let mut start = Vec::new();
    start.extend((packed.len() as u64).to_le_bytes());
    start.extend((h.len() as u64).to_le_bytes());
    start.extend(crc32::hash(&h).to_le_bytes());
    let mut out = b"7z\xbc\xaf\x27\x1c\0\x04".to_vec();
    out.extend(crc32::hash(&start).to_le_bytes());
    out.extend(start);
    out.extend(packed);
    out.extend(h);
    out
}

fn sevenz(d: &[u8]) -> Result<Out, String> {
    unpack(Format::SevenZip, d)
}

fn sz_case(name: &str, d: &[u8]) -> Result<Out, String> {
    case(name, Format::SevenZip, d)
}

#[test]
fn sevenz_builder() {
    let o = sz_case("plain", &sevenz_of(&[F::copy(b"hello")], &[Fl::data("a")])).unwrap();
    assert_eq!(o.files["a"], b"hello");
    let _ = sevenz;
}

/// Items of no stream: empty files, directories, anti-items (left out, silently: they are
/// what an update removes).
#[test]
fn sevenz_empty_and_anti() {
    let files = [
        Fl::data("a"),
        Fl::empty("empty"),
        Fl::dir("dir"),
        Fl::empty("gone").anti(),
        Fl::dir("gonedir").anti(),
        Fl::empty("dir/in"),
    ];
    let o = sz_case("empty and anti", &sevenz_of(&[F::copy(b"data")], &files)).unwrap();
    assert_eq!(o.files.keys().collect::<Vec<_>>(), ["a", "dir/in", "empty"]);
    assert_eq!(o.files["empty"], b"");
    assert_eq!(o.done.tree.entries.len(), 4);
    assert_eq!(o.done.skipped, 0, "{:?}", o.notes);
}

/// More files of data than there are substreams, and more substreams than files: damage, not
/// empty files made of what is missing.
#[test]
fn sevenz_streams_and_files_disagree() {
    let r = sz_case("files without streams", &sevenz_of(&[F::copy(b"data")], &[Fl::data("a"), Fl::data("b")]));
    assert!(r.is_err(), "{:?}", r.map(|o| o.files));
    let two = F { subs: vec![(2, crc32::hash(b"da")), (2, crc32::hash(b"ta"))], ..F::copy(b"data") };
    let r = sz_case("streams without files", &sevenz_of(&[two], &[Fl::data("a")]));
    assert!(r.is_err(), "{:?}", r.map(|o| o.files));
}

/// The same path in two folders: the later one's (a name cannot hold a zero byte in 7z: it
/// ends the name); a path reaching out left out, told.
#[test]
fn sevenz_duplicates_and_outside() {
    let es = [F::copy(b"first"), F::copy(b"second"), F::copy(b"out"), F::copy(b"ok")];
    let files = [Fl::data("a"), Fl::data("a"), Fl::data("../out"), Fl::data("ok")];
    let o = sz_case("duplicates", &sevenz_of(&es, &files)).unwrap();
    assert_eq!(o.files["a"], b"second");
    assert_eq!(o.files["ok"], b"ok");
    assert_eq!(o.files.len(), 2);
    assert_eq!(o.done.skipped, 1, "{:?}", o.notes);
}

/// Folders telling more output than any stream of their coder could make: an error, and no
/// room of that size reserved.
#[test]
fn sevenz_lying_sizes() {
    let t = text();
    let zstd_free: Vec<(&[u8], Vec<u8>, Vec<u8>)> = vec![
        (COPY, vec![], t.clone()),
        (DEFLATE, vec![], {
            // a stored block
            let mut v = vec![1];
            v.extend((t.len() as u16).to_le_bytes());
            v.extend((!(t.len() as u16)).to_le_bytes());
            v.extend(&t);
            v
        }),
        (LZMA, vec![0x5d, 0, 0, 0x10, 0], vec![0; 20]),
        (LZMA2, vec![16], vec![1, 0, 3, b'a', b'b', b'c', b'd', 0]),
        (BZIP2, vec![], vec![b'B', b'Z', b'h', b'9', 0x17, 0x72, 0x45, 0x38, 0x50, 0x90, 0, 0, 0, 0]),
    ];
    for (id, props, pack) in &zstd_free {
        for size in [t.len() as u64 + 1, u32::MAX as u64, 1 << 32, 1 << 40, 1 << 62, u64::MAX - 100, u64::MAX] {
            let f = F { subs: vec![(size, 0)], ..F::of(id, props, pack, size, &t) };
            let name = format!("coder {id:02x?} size {size}");
            let r = sz_case(&name, &sevenz_of(&[f], &[Fl::data("a")]));
            assert!(r.is_err(), "{name}: {:?}", r.map(|o| o.files.len()));
        }
    }
}

/// Two files whose sizes together pass 2^64.
#[test]
fn sevenz_sizes_sum() {
    let f = F { subs: vec![(1 << 63, 0), (1 << 63, 0), (5, 0)], ..F::copy(b"x") };
    let r = sz_case("sizes sum", &sevenz_of(&[f], &[Fl::data("a"), Fl::data("b"), Fl::data("c")]));
    assert!(r.is_err());
}

/// 7-Zip's AES coder of properties telling 2^25 and more rounds of its key (refused: the key
/// would take minutes), and of the raw key (`0x3f`: no rounds) that decrypts.
#[test]
fn sevenz_aes_powers() {
    use aqr_foreign::crypt;
    let data = b"secret data of a file".to_vec();
    let pass = b"pass";
    let iv = [7u8; 16];
    let salt = [9u8; 4];
    // props: byte 0: power, bit 7 a salt, bit 6 an IV; byte 1: their lengths less one
    let props = |power: u8| {
        let mut p = vec![power | 0xc0, (3 << 4) | 15];
        p.extend(salt);
        p.extend(iv);
        p
    };
    let key = crypt::sevenz_key(pass, &salt, 0x3f);
    let enc = crypt::sevenz_aes_encrypt(&key, &iv, &data);
    let mk = |power: u8| {
        sevenz_of(&[F::of(crypt::SEVENZ_AES_ID, &props(power), &enc, data.len() as u64, &data)], &[Fl::data("a")])
    };
    // no rounds at all
    aqr_testkit::case("raw key");
    let a = mk(0x3f);
    let mut l = Opened::open(Format::SevenZip, &a).unwrap();
    assert!(l.encrypted());
    assert!(l.unlock(pass).unwrap());
    let r = Rec::default();
    l.unpack(1, &r).unwrap();
    assert_eq!(r.contents.lock().unwrap().values().next().unwrap(), &data);
    // too many rounds: refused at once
    for power in [25u8, 30, 0x3e] {
        let a = mk(power);
        let r = aqr_testkit::within(Duration::from_secs(20), || {
            let mut l = Opened::open(Format::SevenZip, &a).unwrap();
            let ok = l.unlock(pass);
            (ok, l.unpack(1, &Rec::default()))
        })
        .unwrap();
        assert!(!matches!(r.0, Ok(true)), "power {power}");
        let _ = r.1;
    }
}

/// Every damage of an archive (its start header, header, folders) is an error or a tree, in
/// time.
#[test]
fn sevenz_damaged() {
    let files = [Fl::data("a"), Fl::empty("e"), Fl::dir("d"), Fl::data("d/b")];
    let a = sevenz_of(&[F::copy(&text()), F::copy(b"second")], &files);
    aqr_testkit::damaged(&a, 400, 64, Duration::from_secs(20), |v| {
        let _ = unpack(Format::SevenZip, v);
    });
    let b = aqr_7z::write::solid(&[("a", &text()), ("b", b"bee"), ("e", b"")]);
    aqr_testkit::damaged(&b, 400, 64, Duration::from_secs(20), |v| {
        let _ = unpack(Format::SevenZip, v);
    });
    // the header's CRC and the start header's: each an error
    let mut c = a.clone();
    *c.last_mut().unwrap() ^= 1;
    assert!(sz_case("header crc", &c).is_err());
    let mut c = a.clone();
    c[9] ^= 1;
    assert!(sz_case("start header crc", &c).is_err());
    let mut c = a.clone();
    c[40] ^= 1;
    c.truncate(c.len() - 1);
    assert!(sz_case("cut", &c).is_err());
}
