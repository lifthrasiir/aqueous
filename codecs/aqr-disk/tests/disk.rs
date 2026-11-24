// (an extent of one range is a list of one range, not a range of indices)
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;

use aqr_archive::Evaluate;
use aqr_archive::Level;
use aqr_disk::formula::{self, Options, REST};
use aqr_disk::partition::{self, Partition, Scheme};
use aqr_disk::{File, FileSystem, Image, Piece, Plain};
use aqr_formula::{Formula, Kind, Node};

/// Fixture `name` of tests/data (`scripts/gen-fixtures.sh`), decompressed.
fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

/// A file system of the tests: `TOYFS`, the number of files, then of each the number of its
/// extents and each extent (start and length, of the volume), all u32 LE; the files named
/// `f{i}.bin`, or `f{i}.txt` where `i` is odd.
const TOY: FileSystem =
    FileSystem { name: "toy", starts: |d| d.get(0..5).is_some_and(|m| *m == *b"TOYFS"), files: toy_files };

fn toy_files(d: &dyn Image) -> Vec<File> {
    let mut at = 5;
    let mut u32_at = || {
        let v = d.get(at..at + 4).map(|b| u32::from_le_bytes(b[..].try_into().unwrap()) as usize);
        at += 4;
        v
    };
    let mut out = Vec::new();
    let Some(n) = u32_at() else { return out };
    for i in 0..n.min(1000) {
        let Some(m) = u32_at() else { break };
        let mut extents = Vec::new();
        for _ in 0..m.min(1000) {
            let (Some(s), Some(l)) = (u32_at(), u32_at()) else { break };
            extents.push(s..s + l);
        }
        let ext = if i % 2 == 1 { "txt" } else { "bin" };
        out.push(File { name: format!("f{i}.{ext}").into_bytes(), extents, rank: 0, compressed: None });
    }
    out
}

/// A toy volume of `len` bytes with `files` (their extents), the files' bytes each its index
/// and where it is.
fn toy(len: usize, files: &[&[Range<usize>]]) -> Vec<u8> {
    let mut v = vec![0u8; len];
    let mut table = b"TOYFS".to_vec();
    table.extend((files.len() as u32).to_le_bytes());
    for (i, f) in files.iter().enumerate() {
        table.extend((f.len() as u32).to_le_bytes());
        for r in *f {
            table.extend((r.start as u32).to_le_bytes());
            table.extend((r.len() as u32).to_le_bytes());
            for (k, b) in v[r.clone()].iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(37) ^ (k as u8).wrapping_add(r.start as u8);
            }
        }
    }
    v[..table.len()].copy_from_slice(&table);
    v
}

fn options() -> Options {
    let kind = |name: &[u8]| match name.ends_with(b".txt") {
        true => Kind::Text,
        false => Kind::Unknown,
    };
    Options { file_systems: &[TOY], kind: aqr_formula::Kinds::new(kind), ..Default::default() }
}

fn none(_: &aqr_formula::Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!("no module")
}

/// `f` evaluated, and archived and decoded, both `d`.
fn rebuilds(f: &Formula, d: &[u8]) {
    assert!(f.eval(&none).unwrap() == d, "evaluated");
    let c = aqr_archive::encode(f, None, Level::LOW).unwrap();
    let runner = aqr_archive::natives(&[], &none);
    assert!(aqr_archive::decode(&c, &runner).unwrap() == d, "archived");
}

#[test]
fn partitions_of_hdiutil() {
    let of = |name: &str| partition::partitions(&Plain(&fixture(name)));
    let p = |span: Range<usize>, scheme| Partition { span, scheme };
    assert_eq!(of("gpt.img"), [p(0x800 * 512..0x3800 * 512, Scheme::Gpt)]);
    assert_eq!(of("mbr.img"), [p(512..0x1000 * 512, Scheme::Mbr)]);
    assert_eq!(of("apm.img"), [p(0x40 * 512..0x4000 * 512, Scheme::Apm)]);
}

/// An MBR's 4 entries (type, start, sectors) in sector `at` of `d`.
fn mbr_at(d: &mut [u8], at: usize, entries: &[(u8, u32, u32)]) {
    let s = &mut d[at * 512..][..512];
    s[446..510].fill(0);
    for (i, &(t, start, n)) in entries.iter().enumerate() {
        let e = &mut s[446 + 16 * i..][..16];
        e[4] = t;
        e[8..12].copy_from_slice(&start.to_le_bytes());
        e[12..16].copy_from_slice(&n.to_le_bytes());
    }
    s[510..512].copy_from_slice(&[0x55, 0xaa]);
}

#[test]
fn logical_partitions() {
    let mut d = vec![0u8; 400 * 512];
    // a primary partition, and an extended one of two logical ones: each EBR's next from the
    // extended partition's start, its own from the EBR
    mbr_at(&mut d, 0, &[(0x83, 1, 50), (0x0f, 100, 300)]);
    mbr_at(&mut d, 100, &[(0x07, 2, 40), (0x05, 60, 100)]);
    mbr_at(&mut d, 160, &[(0x07, 1, 20)]);
    let spans = |d: &[u8]| partition::mbr(&Plain(d));
    assert_eq!(spans(&d), [512..51 * 512, 102 * 512..142 * 512, 161 * 512..181 * 512]);
    // a chain back to itself ends
    mbr_at(&mut d, 160, &[(0x07, 1, 20), (0x05, 0, 1)]);
    mbr_at(&mut d, 100, &[(0x07, 2, 40), (0x05, 60, 100)]);
    let mut looped = d.clone();
    mbr_at(&mut looped, 160, &[(0x07, 1, 20), (0x05, 60, 1)]);
    assert_eq!(spans(&looped).len(), 3);
    // not a table: a boot flag of another value
    d[446] = 0x81;
    assert!(spans(&d).is_empty());
}

#[test]
fn not_tables() {
    assert!(partition::partitions(&Plain(&[])).is_empty());
    assert!(partition::partitions(&Plain(&[0; 8192])).is_empty());
    let noise: Vec<u8> = (0..65536u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    assert!(partition::partitions(&Plain(&noise)).is_empty());
    // a volume's boot sector: code where the entries would be
    let mut boot = fixture("mbr.img")[512..][..4096].to_vec();
    boot[446..510].copy_from_slice(&noise[..64]);
    assert!(partition::mbr(&Plain(&boot)).is_empty());
}

#[test]
fn pieces_overlapping_and_contiguous() {
    let d = vec![0u8; 100];
    let file = |name: &str, extents: &[Range<usize>], rank| File {
        name: name.into(),
        extents: extents.to_vec(),
        rank,
        compressed: None,
    };
    let files = [
        // two contiguous extents: one piece
        file("a", &[10..20, 20..30], 2),
        // the same extent, of a lower rank: it names it
        file("b", &[10..30], 0),
        // overlapping the one before: left out
        file("c", &[25..40], 0),
        // past the end: cut
        file("d", &[90..120], 0),
        file("e", &[50..50], 0),
        // pieces out of order, one left out (overlapping `b`): numbered in its order, of those
        // taken
        file("f", &[60..70, 28..35, 40..45], 0),
    ];
    let p: Vec<_> = aqr_disk::pieces(&d, &files).into_iter().map(|p| (p.span, p.file)).collect();
    assert_eq!(
        p,
        [
            (0..10, None),
            (10..30, Some((1, 0))),
            (30..40, None),
            (40..45, Some((5, 1))),
            (45..60, None),
            (60..70, Some((5, 0))),
            (70..90, None),
            (90..100, Some((3, 0)))
        ]
    );
}

/// A file in pieces is one content stream, its pieces split from it; a file of one piece a slice
/// of its own; the image rebuilt, evaluated and archived.
#[test]
fn files_in_pieces() {
    let d = toy(4096, &[&[3000..3500, 100..300, 1000..1100], &[300..700], &[2000..2100, 2200..2300]]);
    let f = formula::encode_with(&d, &options()).unwrap();
    rebuilds(&f, &d);
    let content = |f: &File| f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect::<Vec<_>>();
    let files = toy_files(&Plain(&d));
    // raw (file 0), the rest, file 1 (text), file 2 (raw), as their first pieces come
    let classes: Vec<_> = f.streams.iter().map(|s| (&s.class[..], s.content)).collect();
    assert_eq!(classes, [("raw", true), (REST, false), ("raw", true), ("disk/text", true)]);
    assert!(f.streams[0].data[..] == content(&files[0])[..]);
    assert!(f.streams[2].data[..] == content(&files[2])[..]);
    assert!(f.streams[3].data[..] == content(&files[1])[..]);
    assert_eq!(f.shared.len(), 2);
    assert_eq!(f.shared[0].sizes, [500, 200, 100]);
    assert_eq!(f.shared[0].inputs(), [Node::slice(0, 0, 800)]);
}

/// File systems on partitions: of a GPT, an MBR and an APM, each partition of its own (a toy
/// volume put in place of the one hdiutil made); and on all of an image as well as its partitions.
#[test]
fn volumes_of_tables() {
    for (name, at) in [("gpt.img", 0x800 * 512), ("mbr.img", 512), ("apm.img", 0x40 * 512)] {
        let mut d = fixture(name);
        let v = toy(100_000, &[&[5000..6000], &[7000..8000, 60_000..61_000]]);
        d[at..at + v.len()].copy_from_slice(&v);
        assert!(aqr_disk::starts(&d, &[TOY]), "{name}");
        let files = aqr_disk::files(&d, &[TOY]);
        assert_eq!(files[1].extents, [at + 7000..at + 8000, at + 60_000..at + 61_000], "{name}");
        let f = formula::encode_with(&d, &options()).unwrap();
        rebuilds(&f, &d);
        assert_eq!(f.streams.iter().filter(|s| s.content && !s.data.is_empty()).count(), 2, "{name}");
    }
    // none read: no formula
    assert!(formula::encode_with(&fixture("gpt.img"), &options()).is_none());
    // a volume of all of it, and one of a partition
    let mut d = fixture("mbr.img");
    let v = toy(1000, &[&[500..600]]);
    d[..5].copy_from_slice(b"TOYFS");
    d[5..13].copy_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
    d[13..21].copy_from_slice(&[0, 0x10, 0, 0, 0x10, 0, 0, 0]);
    d[512..512 + v.len()].copy_from_slice(&v);
    let files = aqr_disk::files(&d, &[TOY]);
    assert_eq!(files.iter().map(|f| &f.extents[..]).collect::<Vec<_>>(), [&[0x1000..0x1010][..], &[1012..1112]]);
}

/// Images cut short, and with bytes of their tables changed: no panic, and a formula, where
/// there is one, gives the image back.
#[test]
fn damaged() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (name, at) in [("gpt.img", 0x800 * 512), ("mbr.img", 512), ("apm.img", 0x40 * 512)] {
        let mut d = fixture(name);
        let v = toy(100_000, &[&[5000..6000], &[7000..8000, 60_000..61_000]]);
        d[at..at + v.len()].copy_from_slice(&v);
        for cut in [d.len() / 2, at + 7500, at + 3, 1500, 600] {
            let d = &d[..cut];
            if let Some(f) = formula::encode_with(d, &options()) {
                assert!(f.eval(&none).unwrap() == d, "{name} cut at {cut}");
            }
        }
        // (the tables, and the toy volume's)
        let meta: Vec<usize> = (0..at + 64).filter(|&i| d[i] != 0).collect();
        for _ in 0..300 {
            let mut e = d.clone();
            for _ in 0..1 + next() % 4 {
                let i = meta[(next() % meta.len() as u64) as usize];
                e[i] = next() as u8;
            }
            if let Some(f) = formula::encode_with(&e, &options()) {
                assert!(f.eval(&none).unwrap() == e, "{name} changed");
            }
        }
    }
}

/// Pieces of a file left out where they overlap others: the file's contents are those taken.
#[test]
fn pieces_of_a_file_left_out() {
    let d = toy(4096, &[&[100..200, 300..400, 500..600], &[350..450]]);
    let files = toy_files(&Plain(&d));
    let pieces = aqr_disk::pieces(&d, &files);
    let taken: Vec<&Piece> = pieces.iter().filter(|p| p.file.is_some_and(|f| f.0 == 0)).collect();
    assert_eq!(taken.iter().map(|p| p.span.clone()).collect::<Vec<_>>(), [100..200, 300..400, 500..600]);
    let f = formula::build(&d, &files, &pieces, &options());
    rebuilds(&f, &d);
    let sizes: Vec<usize> = f.streams.iter().filter(|s| s.content).map(|s| s.data.len()).collect();
    assert_eq!(sizes, [300]);
}

/// A WOF stream whose declared length needs far more chunks than its bytes can hold is not taken.
#[test]
fn wof_chunks_of_huge_length() {
    assert!(aqr_disk::compressed::wof_chunks(&[0; 16], 1 << 60, 4096).is_none());
}

/// A GPT of 512-byte sectors with its header's fields and entries `(first, last)` as given.
fn gpt_of(entries_lba: u64, count: u32, size: u32, entries: &[(u64, u64)], len: usize) -> Vec<u8> {
    let mut d = vec![0u8; len];
    let h = &mut d[512..512 + 92];
    h[..8].copy_from_slice(b"EFI PART");
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&count.to_le_bytes());
    h[84..88].copy_from_slice(&size.to_le_bytes());
    if let Some(at) = usize::try_from(entries_lba.saturating_mul(512)).ok().filter(|&a| a < len) {
        for (i, &(first, last)) in entries.iter().enumerate() {
            let Some(e) = d.get_mut(at + i * size as usize..at + i * size as usize + 128) else { break };
            e[..16].fill(1);
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
        }
    }
    d
}

/// GPT fields at the extremes: spans that do not fit in `usize`, an entry array placed at the
/// end of the address space, a count of everything.
#[test]
fn gpt_extremes() {
    let gpt = |d: &[u8]| partition::gpt(&Plain(d));
    let len = 4096 * 4;
    // entries near 2^55 and u64::MAX sectors: out of the image, none
    for lba in [1 << 55, (1 << 55) - 1, u64::MAX >> 9, u64::MAX / 512 + 1, u64::MAX] {
        for size in [128, 4096] {
            for count in [1, u32::MAX] {
                let d = gpt_of(lba, count, size, &[(0, 0)], len);
                let r = aqr_testkit::within(std::time::Duration::from_secs(10), || gpt(&d));
                assert_eq!(r.as_deref(), Ok(&[][..]), "{lba} {size} {count}");
            }
        }
    }
    // partitions at the extremes: those that are not ranges of usize are left out
    let d = gpt_of(2, 8, 128, &[(0, u64::MAX), (u64::MAX, u64::MAX), (1 << 55, (1 << 55) + 5), (3, 2), (4, 4)], len);
    assert_eq!(gpt(&d), [4 * 512..5 * 512]);
    let d = gpt_of(2, 8, 128, &[(0, u64::MAX >> 10), (u64::MAX >> 9, u64::MAX >> 9)], len);
    assert!(gpt(&d).len() <= 2);
}

/// A thousand partitions of one volume: all read (up to the most), the same part of the disk.
#[test]
fn gpt_many_partitions() {
    let all = vec![(100u64, 199u64); 5000];
    let d = gpt_of(2, 5000, 128, &all, 2 * 512 + 5000 * 128 + 512);
    let spans = partition::gpt(&Plain(&d));
    assert!(spans.len() <= 1024 && spans.len() > 100, "{}", spans.len());
    assert!(spans.iter().all(|s| *s == (100 * 512..200 * 512)));
    let d = aqr_disk::formula::encode_with(&d, &options());
    if let Some(f) = d {
        assert!(!f.streams.is_empty());
    }
}

/// A WOF stream of more than 4 GiB of data (chunk table entries of 8 bytes) is read, and its
/// entries of the extremes, or a table of 4-byte ones, are not.
#[test]
fn wof_chunks_over_4_gib() {
    let (chunk, len) = (32768usize, 5u64 << 30);
    let k = (len as usize).div_ceil(chunk);
    let table = (k - 1) * 8;
    let mut s: Vec<u8> = (1..k as u64).flat_map(|i| (i * 3).to_le_bytes()).collect();
    s.resize(table + 3 * k, 0x55);
    let (t, chunks) = aqr_disk::compressed::wof_chunks(&s, len, chunk).unwrap();
    assert_eq!((t, chunks.len()), (0..table, k));
    assert_eq!(chunks[0], (table..table + 3, chunk));
    assert_eq!(chunks.iter().map(|c| c.1 as u64).sum::<u64>(), len);
    // entries past the stream, past usize, or going back
    for v in [u64::MAX, u64::MAX - 7, 1 << 62, 0] {
        let mut e = s.clone();
        e[8 * 100..8 * 100 + 8].copy_from_slice(&v.to_le_bytes());
        let r = aqr_testkit::within(std::time::Duration::from_secs(10), || {
            aqr_disk::compressed::wof_chunks(&e, len, chunk)
        });
        assert!(r.unwrap().is_none(), "{v:#x}");
    }
    // the table cut short
    assert!(aqr_disk::compressed::wof_chunks(&s[..table - 1], len, chunk).is_none());
    // a length of 4 GiB less one byte is a table of 4-byte entries
    assert!(aqr_disk::compressed::wof_chunks(&s, u32::MAX as u64, chunk).is_none());
}

/// Streams of WOF and LZNT1 damaged: none or data of the length declared, within the time.
#[test]
fn compressed_streams_damaged() {
    use aqr_disk::Compressed;
    use aqr_disk::compressed::unpack;
    // two chunks of XPRESS Huffman, stored as they are is no stream: make the bytes of a table of
    // two chunks of data of 4096 and 17 whose chunks are noise
    let noise: Vec<u8> = (0..4200u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
    let mut s = 3000u32.to_le_bytes().to_vec();
    s.extend_from_slice(&noise);
    let how = Compressed::Wof { algorithm: 0, len: 4096 + 17 };
    let lznt = |len: u64| Compressed::Lznt1 { len, unit: 4096, allocated: vec![1200, 1200, 1200] };
    for d in aqr_testkit::damages(s.len(), 200, 64) {
        let e = d.apply(&s);
        let r = aqr_testkit::within(std::time::Duration::from_secs(10), || {
            let a = unpack(&e, &how);
            let b = unpack(e.get(4..).unwrap_or(&[]), &lznt(3 * 4096 - 5));
            let c = unpack(&e, &lznt(u64::MAX));
            (a.map(|u| u.data.len()), b.map(|u| u.data.len()), c.is_none())
        });
        let (a, b, c) = r.unwrap_or_else(|m| panic!("{d}: {m}"));
        assert!(a.is_none_or(|n| n == 4096 + 17), "{d}");
        assert!(b.is_none_or(|n| n == 3 * 4096 - 5), "{d}");
        assert!(c, "{d}");
    }
}
