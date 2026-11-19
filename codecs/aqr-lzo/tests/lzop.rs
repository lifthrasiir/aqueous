//! The structure of lzop files (`lzop::parse`) over headers of every shape: what the flags add,
//! the versions, block sizes that do not hold, files after one another, damage.

use std::time::Duration;

use aqr_lzo::lzo1x::{self, Form, Match, Parse};
use aqr_lzo::lzop::{self, BLOCK_MAX, MAGIC};

const ADLER32_D: u32 = 0x1;
const ADLER32_C: u32 = 0x2;
const EXTRA_FIELD: u32 = 0x40;
const CRC32_D: u32 = 0x100;
const CRC32_C: u32 = 0x200;
const FILTER: u32 = 0x800;

/// A stream of `d`, all of one byte: a literal and a match of the rest.
fn stream(d: &[u8]) -> Vec<u8> {
    let m = Match { lits: 1, len: d.len() as u32 - 1, dist: 1, form: Form::Long };
    lzo1x::encode(d, &Parse { matches: vec![m], tail: 0, first_short: true })
}

enum Body<'a> {
    Stored(&'a [u8]),
    /// A stream of literals only, of the data.
    Lzo(&'a [u8]),
    /// Sizes of the block header as given, and the bytes after it.
    Raw(u32, u32, &'a [u8]),
}

/// An lzop file: `version`, the `flags`, and blocks (each with a checksum of every kind set).
fn file(version: u16, flags: u32, extra: Option<&[u8]>, name: &[u8], blocks: &[Body]) -> Vec<u8> {
    let mut f = MAGIC.to_vec();
    f.extend(version.to_be_bytes());
    f.extend(0x20a0u16.to_be_bytes());
    if version >= 0x0940 {
        f.extend(0x0940u16.to_be_bytes());
    }
    f.push(1);
    if version >= 0x0940 {
        f.push(5);
    }
    f.extend(flags.to_be_bytes());
    if flags & FILTER != 0 {
        f.extend(0u32.to_be_bytes());
    }
    f.extend([0u8; 8]);
    if version >= 0x0940 {
        f.extend([0u8; 4]);
    }
    f.push(name.len() as u8);
    f.extend(name);
    f.extend([0xaa; 4]);
    if flags & EXTRA_FIELD != 0 {
        let e = extra.unwrap_or_default();
        f.extend((e.len() as u32).to_be_bytes());
        f.extend(e);
        f.extend([0xbb; 4]);
    }
    // (a checksum for each flag of the mask)
    let sums = |f: &mut Vec<u8>, mask: u32| {
        for _ in 0..(mask & (ADLER32_D | ADLER32_C | CRC32_D | CRC32_C)).count_ones() {
            f.extend([0xcc; 4]);
        }
    };
    for b in blocks {
        match b {
            Body::Stored(d) => {
                f.extend((d.len() as u32).to_be_bytes());
                f.extend((d.len() as u32).to_be_bytes());
                sums(&mut f, flags & (ADLER32_D | CRC32_D));
                f.extend(*d);
            }
            Body::Lzo(d) => {
                let s = stream(d);
                assert!(s.len() < d.len(), "{} < {}", s.len(), d.len());
                f.extend((d.len() as u32).to_be_bytes());
                f.extend((s.len() as u32).to_be_bytes());
                sums(&mut f, flags);
                f.extend(s);
            }
            Body::Raw(d, s, bytes) => {
                f.extend(d.to_be_bytes());
                f.extend(s.to_be_bytes());
                sums(&mut f, flags);
                f.extend(*bytes);
            }
        }
    }
    f.extend(0u32.to_be_bytes());
    f
}

const NEW: u16 = 0x1040;
const OLD: u16 = 0x0930;

fn text() -> Vec<u8> {
    vec![b'a'; 100]
}

#[test]
fn flags_each_count_their_checksum() {
    let t = text();
    let blocks = [Body::Lzo(&t), Body::Stored(b"abc"), Body::Lzo(&t)];
    for flags in [
        0,
        ADLER32_D,
        CRC32_D,
        ADLER32_D | CRC32_D,
        ADLER32_D | ADLER32_C,
        CRC32_D | CRC32_C,
        ADLER32_D | CRC32_D | ADLER32_C | CRC32_C,
        ADLER32_D | CRC32_C,
        ADLER32_C,
    ] {
        let f = file(NEW, flags, None, b"x", &blocks);
        let p = lzop::parse(&f).unwrap_or_else(|| panic!("flags {flags:#x}"));
        assert_eq!(p.len, f.len(), "flags {flags:#x}");
        assert_eq!(p.blocks.len(), 3, "flags {flags:#x}");
        assert_eq!((p.blocks[0].data, p.blocks[1].data), (100, 3), "flags {flags:#x}");
        assert!(p.blocks[1].stored() && !p.blocks[0].stored());
        // the blocks' streams are where they say
        for b in [p.blocks[0], p.blocks[2]] {
            assert_eq!(lzo1x::decode(&f[b.at..b.at + b.stream], 1 << 20).unwrap().data, t);
        }
        assert_eq!(&f[p.blocks[1].at..p.blocks[1].at + 3], b"abc");
    }
}

#[test]
fn header_variants() {
    let t = text();
    let blocks = [Body::Lzo(&t)];
    for version in [OLD, 0x0900, NEW, 0x0940] {
        for extra in [None, Some(&b""[..]), Some(&b"extra field"[..])] {
            for filter in [0, FILTER] {
                for name in [&b""[..], b"name", &[b'n'; 255][..]] {
                    let flags = filter | if extra.is_some() { EXTRA_FIELD } else { 0 } | CRC32_D;
                    let f = file(version, flags, extra, name, &blocks);
                    let p = lzop::parse(&f).unwrap_or_else(|| panic!("{version:#x} {extra:?} {filter:#x}"));
                    assert_eq!((p.method, p.len, p.blocks.len()), (1, f.len(), 1));
                    assert_eq!(lzo1x::decode(&f[p.blocks[0].at..][..p.blocks[0].stream], 1 << 20).unwrap().data, t);
                }
            }
        }
    }
}

#[test]
fn versions_and_methods_refused() {
    let t = text();
    let ok = file(NEW, 0, None, b"", &[Body::Lzo(&t)]);
    assert!(lzop::parse(&ok).is_some());
    // a version before 0.90, a method that is not LZO1X's
    assert!(lzop::parse(&file(0x0899, 0, None, b"", &[Body::Lzo(&t)])).is_none());
    let method = 9 + 2 + 2 + 2;
    for m in [0u8, 4, 0x80, 0xff] {
        let mut f = ok.clone();
        f[method] = m;
        assert!(lzop::parse(&f).is_none(), "method {m}");
    }
    assert!(lzop::parse(&ok[..8]).is_none());
    assert!(lzop::parse(b"").is_none());
}

#[test]
fn block_sizes_that_do_not_hold() {
    let t = text();
    let s = stream(&t);
    let b = |d: u32, s_: u32, bytes: &[u8]| file(NEW, 0, None, b"", &[Body::Raw(d, s_, bytes)]);
    // sound, to begin with
    assert!(lzop::parse(&b(100, s.len() as u32, &s)).is_some());
    // a stream longer than its data, none, past the file, past the bound
    assert!(lzop::parse(&b(10, 11, &[0; 11])).is_none());
    assert!(lzop::parse(&b(10, 0, &[])).is_none());
    assert!(lzop::parse(&b(100, s.len() as u32 + 1, &s)).is_none());
    assert!(lzop::parse(&b(100, 99, &s)).is_none());
    assert!(lzop::parse(&b(BLOCK_MAX as u32 + 1, 5, &[0; 5])).is_none());
    assert!(lzop::parse(&b(u32::MAX, u32::MAX, &[])).is_none());
    assert!(lzop::parse(&b(u32::MAX, 5, &[0; 5])).is_none());
    // the data's size at the bound is taken, with a stream not of that much
    assert!(lzop::parse(&b(BLOCK_MAX as u32, 5, &[0; 5])).is_some());
    // no end marker
    let mut f = file(NEW, 0, None, b"", &[Body::Lzo(&t)]);
    f.truncate(f.len() - 4);
    assert!(lzop::parse(&f).is_none());
    // a header whose lengths run past the file: the extra field's, the name's
    let f = file(NEW, EXTRA_FIELD, Some(&[1; 10]), b"n", &[]);
    let at = 9 + 2 + 2 + 2 + 2 + 4 + 8 + 4 + 1 + 1 + 4;
    let mut g = f.clone();
    g[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(lzop::parse(&g).is_none());
    let mut g = f.clone();
    g[at - 6] = 255;
    assert!(lzop::parse(&g).is_none());
}

#[test]
fn files_after_one_another() {
    let t = text();
    let one = file(NEW, CRC32_D, None, b"a", &[Body::Lzo(&t), Body::Stored(b"xy")]);
    let two = file(OLD, ADLER32_D, None, b"bb", &[Body::Stored(b"z")]);
    let both = [&one[..], &two[..]].concat();
    let p = lzop::parse(&both).unwrap();
    assert_eq!(p.len, one.len());
    // the second one starts after the first's end marker
    let q = lzop::parse(&both[p.len..]).unwrap();
    assert_eq!(p.len + q.len, both.len());
    // bytes that are not a file after one
    let junk = [&one[..], b"trailing bytes"].concat();
    assert_eq!(lzop::parse(&junk).unwrap().len, one.len());
}

#[test]
fn damaged_files() {
    let t = text();
    let f = file(
        NEW,
        ADLER32_D | CRC32_D | EXTRA_FIELD | FILTER,
        Some(b"ex"),
        b"nm",
        &[Body::Lzo(&t), Body::Stored(b"abc")],
    );
    aqr_testkit::damaged(&f, 64, f.len(), Duration::from_secs(10), |d| {
        if let Some(p) = lzop::parse(d) {
            assert!(p.len <= d.len() && p.header <= p.len);
            for b in &p.blocks {
                assert!(b.at + b.stream <= d.len() && b.stream <= b.data && b.data <= BLOCK_MAX);
            }
        }
    });
}
