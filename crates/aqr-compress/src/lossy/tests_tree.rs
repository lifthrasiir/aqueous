//! `slack` of region files, `coding` of gzip files, the specs of `--lossy`, the mark, and
//! archives of trees of files rewritten.

use super::*;
use crate::{Input, compress_tree};

/// Bytes like text, `n` of them, of words chosen by `seed`.
fn text(n: usize, seed: u64) -> Vec<u8> {
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa"];
    let (mut t, mut x) = (Vec::with_capacity(n + 16), seed);
    while t.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        t.extend_from_slice(words[(x >> 60) as usize % words.len()].as_bytes());
        t.extend_from_slice(format!(" {} ", (x >> 40) % 300).as_bytes());
    }
    t.truncate(n);
    t
}

/// A region file of a chunk of two sectors at sector 2, a sector freed (3 is the chunk's, 4
/// freed, of stale bytes) and a chunk of one at 5, then stale bytes past the last sector:
/// its slack filled with `stale` bytes (the header's timestamps too, which are kept).
fn region(stale: u8) -> Vec<u8> {
    const S: usize = 4096;
    let mut d = vec![stale; 6 * S + 100];
    d[..2 * S].fill(0);
    let mut chunk = |index: usize, sector: usize, count: usize, data: &[u8]| {
        d[4 * index..4 * index + 4].copy_from_slice(&((sector as u32) << 8 | count as u32).to_be_bytes());
        d[S + 4 * index..S + 4 * index + 4].copy_from_slice(&(1_700_000_000u32 + index as u32).to_be_bytes());
        let at = sector * S;
        d[at..at + 4].copy_from_slice(&(data.len() as u32 + 1).to_be_bytes());
        d[at + 4] = aqr_minecraft::region::ZLIB;
        d[at + 5..at + 5 + data.len()].copy_from_slice(data);
    };
    let zlib = |d: &[u8]| {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, d).unwrap();
        z.finish().unwrap()
    };
    let (a, b) = (zlib(&text(30_000, 1)), zlib(&text(2_000, 2)));
    assert!(a.len() > S && a.len() + 5 < 2 * S && b.len() + 5 < S, "{} {}", a.len(), b.len());
    chunk(0, 2, 2, &a);
    chunk(33, 5, 1, &b);
    d
}

#[test]
fn region_slack_zeroed() {
    let x = region(0xa5);
    assert_eq!(detect(&x), Some(Format::Mca));
    let opt = Options::default();
    let Some((Rewrite::Made { format, mark, len }, Some(r))) = rewrite(&x, &Lossy::parse("slack").unwrap(), &opt)
    else {
        panic!("rewritten")
    };
    assert_eq!(
        (format, &mark.classes[..], mark.size, len),
        (Format::Mca, &["slack".to_string()][..], x.len() as u64, r.len() as u64)
    );
    assert_eq!(r.len(), x.len());
    // the chunks and the header as they were, everything else zero
    assert_eq!(r[..2 * 4096], x[..2 * 4096]);
    assert_eq!(r, region(0));
    check::mca(&x, &r).unwrap();
    // (none to zero: nothing rewritten)
    assert!(rewrite(&r, &Lossy::parse("slack").unwrap(), &opt).is_none());
    // (not taken by coding alone)
    assert!(rewrite(&x, &Lossy::parse("coding").unwrap(), &opt).is_none());
    // a check of its own: a chunk changed, or slack made other than zeros, fails
    for (at, what) in
        [(2 * 4096 + 100, "a chunk changed"), (4 * 4096 + 7, "but to zeros"), (6 * 4096 + 3, "but to zeros")]
    {
        let mut bad = r.clone();
        bad[at] = 1;
        assert!(check::mca(&x, &bad).unwrap_err().contains(what), "{at}");
    }
    let mut bad = r.clone();
    bad[4100] ^= 1;
    assert!(check::mca(&x, &bad).is_err());
}

/// A gzip member of `data` by `encoder` (a flate-zoo spec), named.
fn member(data: &[u8], encoder: &str) -> Vec<u8> {
    let mut m = vec![0x1f, 0x8b, 8, 8, 1, 2, 3, 4, 2, 3];
    m.extend_from_slice(b"t.txt\0");
    m.extend(encoder.parse::<flate_zoo::Encoder>().unwrap().compress(data).unwrap());
    let mut crc = flate2::Crc::new();
    crc.update(data);
    m.extend(crc.sum().to_le_bytes());
    m.extend((data.len() as u32).to_le_bytes());
    m
}

#[test]
fn gzip_streams_of_others_made_again() {
    let (a, b) = (text(200_000, 3), text(50_000, 4));
    let (ma, mb) = (member(&a, "libdeflate:level=9"), member(&b, "zlib:level=9"));
    let x = [ma.clone(), mb.clone()].concat();
    let opt = super::tests::high();
    let Some((Rewrite::Made { mark, .. }, Some(r))) = rewrite(&x, &Lossy::parse("coding:gzip").unwrap(), &opt) else {
        panic!("rewritten")
    };
    assert_eq!((&mark.classes[..], mark.size), (&["coding".to_string()][..], x.len() as u64));
    check::gzip(&x, &r).unwrap();
    // the member of zlib's kept as it is, the other's stream zlib's at level 6
    assert!(r.ends_with(&mb));
    assert_eq!(r[..16], ma[..16]);
    let stream = aqr_flate::zlib::compress(&a, aqr_flate::zlib::Params::default()).unwrap();
    assert_eq!(r[16..16 + stream.len()], stream[..]);
    assert_eq!(r.len(), 16 + stream.len() + 8 + mb.len());
    let mut got = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::MultiGzDecoder::new(&r[..]), &mut got).unwrap();
    assert_eq!(got, [&a[..], &b[..]].concat());
    // (zlib's alone: nothing to change)
    assert!(rewrite(&mb, &Lossy::parse("coding").unwrap(), &opt).is_none());
    // (below level 7, any stream but zlib's at level 6 made again: zlib's at 9 too)
    let Some((Rewrite::Made { .. }, Some(r5))) = rewrite(&x, &Lossy::parse("coding").unwrap(), &Options::default())
    else {
        panic!("rewritten at the default level")
    };
    check::gzip(&x, &r5).unwrap();
    let b6 = aqr_flate::zlib::compress(&b, PARAMS).unwrap();
    assert_eq!(r5.len(), 2 * (16 + 8) + stream.len() + b6.len());
    // (below level 4, where DEFLATE streams are kept as they are: kept, told why)
    let low = Options { level: aqr_archive::Level::LOW, ..Options::default() };
    assert!(matches!(rewrite(&x, &Lossy::parse("coding").unwrap(), &low), Some((Rewrite::Kept { .. }, None))));
    // (data after the members: kept as it is, told why)
    let tail = [x.clone(), vec![0; 16]].concat();
    assert!(matches!(rewrite(&tail, &Lossy::parse("coding").unwrap(), &opt), Some((Rewrite::Kept { .. }, None))));
    // a check of its own: other data, header or trailer fails
    for at in [3, 40, r.len() - mb.len() - 3] {
        let mut bad = r.clone();
        bad[at] ^= 0x10;
        assert!(check::gzip(&x, &bad).is_err(), "{at}");
    }
}

#[test]
fn specs() {
    let all = Lossy::parse("all").unwrap();
    assert_eq!(all.classes().collect::<Vec<_>>(), [Class::Coding, Class::Slack]);
    assert_eq!(Lossy::parse("").unwrap(), Lossy::parse("coding").unwrap());
    let some = Lossy::parse("coding:zip,slack").unwrap();
    assert!(some.allows(Class::Coding, Format::Zip) && !some.allows(Class::Coding, Format::Gzip));
    assert!(some.allows(Class::Slack, Format::Mca));
    assert_eq!(Lossy::parse("coding:zip,coding").unwrap(), Lossy::parse("coding").unwrap());
    let both = Lossy::parse("coding:zip,coding:gzip").unwrap();
    assert!(both.allows(Class::Coding, Format::Zip) && both.allows(Class::Coding, Format::Gzip));
    for bad in ["cod", "coding:png", "slack:zip", "layout", "all:zip", "coding:"] {
        assert!(Lossy::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn mark_read_back() {
    for m in [
        Mark { classes: vec!["coding".into()], size: 0 },
        Mark { classes: vec!["coding".into(), "slack".into()], size: 1 << 40 },
        Mark { classes: vec!["slack".into()], size: 300 },
    ] {
        let x = m.extension();
        assert_eq!(&x.0[..], KEY.as_bytes());
        assert_eq!(Mark::of(&[x]), Some(m));
    }
    // {0: ["coding"], 1: 5, 2: h'00'}: a hash skipped
    let v: Arc<[u8]> = [&[0xa3, 0x00, 0x81, 0x66][..], b"coding", &[0x01, 0x05, 0x02, 0x41, 0x00]].concat().into();
    assert_eq!(Mark::of(&[(KEY.as_bytes().into(), v)]), Some(Mark { classes: vec!["coding".into()], size: 5 }));
    for bad in [&[0xa1, 0x01, 0x05][..], &[0xa2, 0x00, 0x80, 0x01, 0x05], &[0xa2, 0x00, 0x81, 0x66]] {
        assert_eq!(Mark::of(&[(KEY.as_bytes().into(), bad.into())]), None, "{bad:?}");
    }
}

/// An archive of a region file, a gzip file and text with [`Options::lossy`]: the first two
/// given back as R, marked with the size of X; the text as it is, unmarked. Without it, all as
/// they are.
#[test]
fn tree_of_files_rewritten() {
    let (mca, t) = (region(0x5a), text(40_000, 5));
    let gz = member(&text(100_000, 6), "libdeflate:level=12");
    let names = ["r.0.0.mca", "a.txt.gz", "b.txt"];
    let datas: [&[u8]; 3] = [&mca, &gz, &t];
    let tree = aqr_archive::Tree {
        entries: (0..3)
            .map(|c| aqr_archive::Entry {
                parent: None,
                name: names[c].as_bytes().to_vec(),
                node: aqr_archive::EntryNode::File(c),
                extra: Default::default(),
            })
            .collect(),
        ..Default::default()
    };
    let inputs: Vec<Input> = (0..3).map(|c| Input { name: names[c].as_bytes(), data: datas[c] }).collect();
    for segment in [0, 1] {
        let opt = Options { threads: 2, lossy: Some(Lossy::parse("all").unwrap()), segment, ..super::tests::high() };
        let a = compress_tree(&tree, &inputs, &opt);
        assert!(a.report.unverified.is_empty());
        let aqr_archive::Value::Tree(got_tree, got) = aqr_decompress::decompress_value(&a.archive, 1).unwrap() else {
            panic!("a tree")
        };
        let (rewritten, mut r) = (&a.report.files, Vec::new());
        for (c, &x) in datas.iter().enumerate() {
            let e = got_tree.entries.iter().find(|e| e.name == names[c].as_bytes()).unwrap();
            let aqr_archive::EntryNode::File(k) = e.node else { panic!("a file") };
            let mark = Mark::of(&e.extra.extensions);
            match c {
                2 => {
                    assert_eq!(got[k], x);
                    assert_eq!((mark, &rewritten[c].lossy), (None, &None));
                }
                _ => {
                    assert_ne!(got[k], x);
                    let class = if c == 0 { "slack" } else { "coding" };
                    assert_eq!(mark, Some(Mark { classes: vec![class.into()], size: x.len() as u64 }), "{segment}");
                    assert!(
                        matches!(&rewritten[c].lossy, Some(Rewrite::Made { len, .. }) if *len == got[k].len() as u64)
                    );
                    assert_eq!(rewritten[c].len, got[k].len() as u64);
                    r.push(got[k].clone());
                }
            }
        }
        check::mca(&mca, &r[0]).unwrap();
        check::gzip(&gz, &r[1]).unwrap();
    }
    let a = compress_tree(&tree, &inputs, &Options { threads: 2, ..Options::default() });
    let aqr_archive::Value::Tree(got_tree, got) = aqr_decompress::decompress_value(&a.archive, 1).unwrap() else {
        panic!("a tree")
    };
    assert!(got_tree.entries.iter().all(|e| e.extra.extensions.is_empty()));
    assert!(datas.iter().all(|x| got.iter().any(|g| g == x)));
}

/// At the default level, a file rewritten archives smaller than as it was (its bytes, not the
/// modules'): its streams, zlib's
/// at level 6, are coded at the adaptive profile with [`PARAMS`] hinted ([`Rewritten`]), their
/// records a few bytes; those of libdeflate and of zlib at level 9 some 10% of them otherwise.
#[test]
fn default_level_pays() {
    assert_eq!(PARAMS, aqr_flate::zlib::Params::default());
    for encoder in ["libdeflate:level=9", "zlib:level=9"] {
        let x = member(&text(300_000, 7), encoder);
        let tree = aqr_archive::Tree {
            entries: vec![aqr_archive::Entry {
                parent: None,
                name: b"a.txt.gz".to_vec(),
                node: aqr_archive::EntryNode::File(0),
                extra: Default::default(),
            }],
            ..Default::default()
        };
        let inputs = [Input { name: b"a.txt.gz", data: &x }];
        let size = |lossy: Option<&str>| {
            let opt = Options { threads: 2, lossy: lossy.map(|l| Lossy::parse(l).unwrap()), ..Options::default() };
            let a = compress_tree(&tree, &inputs, &opt);
            assert!(a.report.unverified.is_empty());
            // (the file's bytes: not the module blocks, which an archive of many has once)
            a.report.files[0].archived
        };
        let (was, is) = (size(None), size(Some("coding")));
        assert!(is < 0.95 * was, "{encoder}: {was} -> {is}");
    }
}

/// A SQLite database with a journal archived along (`NAME-journal`, `NAME-wal`, not empty) is
/// kept as it is, told why: a rollback could need what its free pages hold. An empty journal
/// does not count.
#[test]
fn database_with_journal_kept() {
    let mut db = vec![0u8; 2 * 4096];
    db[..16].copy_from_slice(b"SQLite format 3\0");
    db[16..18].copy_from_slice(&4096u16.to_be_bytes());
    (db[18], db[19], db[21], db[22], db[23]) = (1, 1, 64, 32, 32);
    assert_eq!(detect(&db), Some(Format::Sqlite));
    for (journal, data, kept) in
        [("a.db-journal", &b"journal"[..], true), ("a.db-wal", b"wal", true), ("a.db-journal", b"", false)]
    {
        let names = [&b"a.db"[..], journal.as_bytes()];
        let datas = [&db[..], data];
        let inputs: Vec<Input> = (0..2).map(|c| Input { name: names[c], data: datas[c] }).collect();
        let opt = Options { lossy: Some(Lossy::parse("slack").unwrap()), ..Options::default() };
        let rw = Rewriting::new(&inputs[..], &opt);
        let r = crate::Source::read(&rw, 0).unwrap();
        assert_eq!(r[..], db[..]);
        let made = rw.made.lock().unwrap();
        match (made.get(&0), kept) {
            (Some(Rewrite::Kept { reason, .. }), true) => assert!(reason.contains(&journal[5..]), "{reason}"),
            (Some(Rewrite::Kept { reason, .. }), false) => assert!(!reason.contains("archived along"), "{reason}"),
            (w, _) => assert!(!kept, "{journal}: {w:?}"),
        }
    }
}
