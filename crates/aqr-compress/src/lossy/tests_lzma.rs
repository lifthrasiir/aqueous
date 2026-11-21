//! `coding` of `.lzma` and `.xz` files: fixtures of liblzma (XZ Utils), the rewriters' and the
//! checks' behaviour on them, and an archive of a tree.

use super::*;
use crate::{Input, compress_tree};
use liblzma::stream::{Check, Filters, LzmaOptions, MtStreamBuilder, Stream};
use std::io::{Read, Write};

/// `n` bytes of source text (this crate's files, from a place `seed` picks), with an x86 call's
/// opcode here and there for the BCJ filter.
fn text(n: usize, seed: u64) -> Vec<u8> {
    let files: [&[u8]; 7] = [
        include_bytes!("../archive.rs"),
        include_bytes!("../tests.rs"),
        include_bytes!("../exe.rs"),
        include_bytes!("../formats.rs"),
        include_bytes!("../detect.rs"),
        include_bytes!("../made.rs"),
        include_bytes!("../output.rs"),
    ];
    let all = files.concat();
    let from = (seed as usize * 7919) % all.len();
    let mut t: Vec<u8> = all.iter().cycle().skip(from).take(n).copied().collect();
    for i in (100..n).step_by(211) {
        t[i] = 0xe8;
    }
    t
}

fn encode(stream: Stream, data: &[u8]) -> Vec<u8> {
    let mut e = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn xz(data: &[u8], preset: u32, check: Check) -> Vec<u8> {
    encode(Stream::new_easy_encoder(preset, check).unwrap(), data)
}

fn unxz(d: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut v = Vec::new();
    liblzma::read::XzDecoder::new_multi_decoder(d).read_to_end(&mut v)?;
    Ok(v)
}

fn lzma_alone(data: &[u8]) -> Vec<u8> {
    let o = LzmaOptions::new_preset(6).unwrap();
    encode(Stream::new_lzma_encoder(&o).unwrap(), data)
}

fn unlzma(d: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut v = Vec::new();
    let s = Stream::new_lzma_decoder(u64::MAX).unwrap();
    liblzma::read::XzDecoder::new_stream(d, s).read_to_end(&mut v)?;
    Ok(v)
}

/// R of `x`, which must be made.
fn made(x: &[u8]) -> Vec<u8> {
    match rewrite(x, &coding(), &super::tests::high()) {
        Some((Rewrite::Made { .. }, Some(r))) => r,
        other => panic!("not made: {:?}", other.map(|o| o.0)),
    }
}

fn kept(x: &[u8]) -> String {
    match rewrite(x, &coding(), &super::tests::high()) {
        Some((Rewrite::Kept { reason, .. }, None)) => reason,
        other => panic!("not kept: {:?}", other.map(|o| o.0)),
    }
}

#[test]
fn lzma_alone_made_again() {
    let data = text(400_000, 1);
    let x = lzma_alone(&data);
    assert_eq!(detect(&x), Some(Format::Lzma));
    let r = made(&x);
    check::lzma(&x, &r).unwrap();
    assert_eq!(unlzma(&r).unwrap(), data);
    assert_eq!(r[..13], x[..13]);
    eprintln!("lzma: X {} R {}", x.len(), r.len());
    assert!(r.len() * 100 <= x.len() * 115);
    // (canonical: again gives nothing)
    assert!(rewrite(&r, &coding(), &super::tests::high()).is_none());
    // (data after the stream)
    assert_eq!(kept(&[&x[..], &[0, 0, 0, 0]].concat()), "data after the stream");
    // a check of its own: the data, the header
    for at in [20, r.len() / 2, 5, 1] {
        let mut bad = r.clone();
        bad[at] ^= 0x10;
        assert!(check::lzma(&x, &bad).is_err(), "{at}");
    }
    assert!(check::lzma(&x, &[&r[..], &[0]].concat()).is_err());
    assert!(check::lzma(&x, &r[..r.len() - 3]).is_err());
}

fn assert_xz(x: &[u8], data: &[u8], name: &str, max_percent: usize) -> Vec<u8> {
    assert_eq!(detect(x), Some(Format::Xz));
    assert_eq!(unxz(x).unwrap(), data);
    let r = made(x);
    check::xz(x, &r).unwrap();
    assert_eq!(unxz(&r).unwrap(), data, "{name}");
    eprintln!("xz {name}: X {} R {}", x.len(), r.len());
    assert!(r.len() * 100 <= x.len() * max_percent, "{name}: {} {}", x.len(), r.len());
    assert!(rewrite(&r, &coding(), &super::tests::high()).is_none(), "{name}");
    r
}

#[test]
fn xz_streams_made_again() {
    let data = text(500_000, 2);
    for (name, preset, check) in [
        ("6 crc32", 6, Check::Crc32),
        ("6 crc64", 6, Check::Crc64),
        ("9e sha256", 9 | liblzma::stream::PRESET_EXTREME, Check::Sha256),
        ("6 none", 6, Check::None),
    ] {
        assert_xz(&xz(&data, preset, check), &data, name, 115);
    }
}

#[test]
fn xz_blocks_and_streams() {
    let data = text(600_000, 3);
    // blocks of 100 KB, the compressed size in each header
    let mut b = MtStreamBuilder::new();
    b.preset(6).check(Check::Crc64).threads(2).block_size(100_000);
    let multi = encode(b.encoder().unwrap(), &data);
    let r = assert_xz(&multi, &data, "blocks", 115);
    assert_eq!(r[..12], multi[..12]);
    // two streams, padded, the second of another check
    let other = text(100_000, 4);
    let x = [xz(&data, 6, Check::Crc64), vec![0; 8], xz(&other, 1, Check::Crc32)].concat();
    let r = made(&x);
    check::xz(&x, &r).unwrap();
    assert_eq!(unxz(&r).unwrap(), [&data[..], &other[..]].concat());
    let at = r.windows(8).position(|w| w == [0; 8]).unwrap();
    assert_eq!(r[at + 8..at + 14], x[x.len() - xz(&other, 1, Check::Crc32).len()..][..6]);
    // BCJ before LZMA2
    let mut f = Filters::new();
    f.x86().lzma2(&LzmaOptions::new_preset(6).unwrap());
    let bcj = encode(Stream::new_stream_encoder(&f, Check::Crc64).unwrap(), &data);
    assert_xz(&bcj, &data, "x86", 115);
    // data after the streams, a stream cut, a chain not ending with LZMA2
    let x = xz(&data, 6, Check::Crc64);
    assert_eq!(kept(&[&x[..], b"junk"].concat()), "data after the streams");
    assert_eq!(kept(&[&x[..], &[0; 3]].concat()), "data after the streams");
    // a chain ending with another filter (a header with its filter ID changed, its CRC-32 made again)
    let mut bad = x.clone();
    let size = (bad[12] as usize + 1) * 4;
    let id = bad[12..12 + size].iter().position(|&b| b == 0x21).unwrap();
    bad[12 + id] = 0x03;
    let mut crc = flate2::Crc::new();
    crc.update(&bad[12..12 + size - 4]);
    bad[12 + size - 4..12 + size].copy_from_slice(&crc.sum().to_le_bytes());
    assert_eq!(kept(&bad), "a block of a filter chain not ending with LZMA2");
}

#[test]
fn xz_checks_of_their_own() {
    let data = text(300_000, 5);
    let mut b = MtStreamBuilder::new();
    b.preset(6).check(Check::Crc64).threads(2).block_size(100_000);
    let x = encode(b.encoder().unwrap(), &data);
    let r = made(&x);
    check::xz(&x, &r).unwrap();
    let n = r.len();
    // the stream header's check type, the compressed data, the index, a header's size, the footer
    let first_block = 12;
    let uncompressed_field = r[first_block..].iter().position(|&b| b == 0x21).unwrap();
    for (what, at) in [
        ("flags", 7),
        ("data", 300),
        ("data", n / 2),
        ("index", n - 12 - 6),
        ("footer", n - 5),
        ("header", first_block + uncompressed_field + first_block - 1),
        ("check", n - 12 - 24),
    ] {
        let mut bad = r.clone();
        bad[at] ^= 0x01;
        assert!(check::xz(&x, &bad).is_err(), "{what} {at}");
    }
    // (a stream cut, bytes after it, a block's uncompressed size changed with CRCs made right)
    assert!(check::xz(&x, &r[..n - 1]).is_err());
    assert!(check::xz(&x, &[&r[..], &[0; 4]].concat()).is_err());
    assert!(check::xz(&x, &made(&xz(&data, 6, Check::Crc64))).is_err());
}

/// The files `names` of the contents `files` archived with and without `--lossy`: each is given
/// back as its R, marked; the bytes of the archives.
fn archived(names: &[&str], files: &[Vec<u8>], what: &str) -> (usize, usize) {
    let entries = names.iter().enumerate().map(|(c, n)| aqr_archive::Entry {
        parent: None,
        name: n.as_bytes().to_vec(),
        node: aqr_archive::EntryNode::File(c),
        extra: Default::default(),
    });
    let tree = aqr_archive::Tree { entries: entries.collect(), ..Default::default() };
    let inputs: Vec<Input> = names.iter().zip(files).map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let plain = compress_tree(&tree, &inputs, &super::tests::high());
    let opt = Options { lossy: Some(coding()), ..super::tests::high() };
    let a = compress_tree(&tree, &inputs, &opt);
    assert!(a.report.unverified.is_empty());
    let aqr_archive::Value::Tree(got_tree, got) = aqr_decompress::decompress_value(&a.archive, 1).unwrap() else {
        panic!("a tree")
    };
    for (c, x) in files.iter().enumerate() {
        let e = got_tree.entries.iter().find(|e| e.name == names[c].as_bytes()).unwrap();
        let aqr_archive::EntryNode::File(k) = e.node else { panic!("a file") };
        // (R where it archived smaller, else X, made again as it was)
        match &a.report.files[c].lossy {
            Some(Rewrite::Made { .. }) => {
                assert_eq!(got[k], made(x));
                assert!(Mark::of(&e.extra.extensions).is_some());
            }
            Some(Rewrite::Kept { reason, .. }) if reason.contains("no smaller") => {
                assert_eq!(got[k], *x);
                assert!(Mark::of(&e.extra.extensions).is_none());
            }
            w => panic!("{what}: {w:?}"),
        }
    }
    eprintln!("{what}: archive {} with --lossy, {} without", a.archive.len(), plain.archive.len());
    (a.archive.len(), plain.archive.len())
}

#[test]
fn xz_tree_archived() {
    let data = text(700_000, 6);
    let (a, b) = (xz(&data, 6, Check::Crc64), xz(&data, 1, Check::Crc64));
    // one file: R about 10% larger (XZ Utils' parser, not the canonical one) where no
    // recompression pays, so made again as it was: nothing lost
    let (with, without) = archived(&["a.xz"], std::slice::from_ref(&a), "xz, one file");
    assert!(with <= without, "{with} {without}");
    // the same data of two encoders: one R's worth of archive
    let (with, without) = archived(&["a.xz", "b.xz"], &[a.clone(), b.clone()], "xz, two encoders");
    assert!(with <= without, "{with} {without}");
}

#[test]
fn lzma_tree_archived() {
    let data = text(700_000, 7);
    let x = lzma_alone(&data);
    let (with, without) = archived(&["a.lzma"], &[x], "lzma, one file");
    assert!(with <= without, "{with} {without}");
}

/// A block header with its compressed size (as XZ Utils' threaded encoder writes it) whose
/// field takes fewer bytes in R: the header padded otherwise, its filter flags the same.
#[test]
fn xz_block_header_with_sizes() {
    let data: Vec<u8> = (0..600_000u32).flat_map(|i| format!("{} {} ", i % 977, i % 13).into_bytes()).collect();
    let opts = liblzma::stream::MtStreamBuilder::new()
        .threads(2)
        .block_size(1 << 20)
        .preset(6)
        .check(liblzma::stream::Check::Crc64)
        .encoder()
        .unwrap();
    let mut x = Vec::new();
    std::io::Write::write_all(&mut liblzma::write::XzEncoder::new_stream(&mut x, opts), &data).unwrap();
    // (the headers tell the compressed size)
    assert!(x[12 + 1] & 0x40 != 0, "{:02x}", x[13]);
    let opt = Options { level: aqr_archive::Level::HIGH, ..Options::default() };
    let Some((Rewrite::Made { .. }, Some(r))) = rewrite(&x, &coding(), &opt) else {
        panic!("rewritten: {:?}", rewrite(&x, &coding(), &opt).map(|r| r.0))
    };
    check::xz(&x, &r).unwrap();
}

/// `coding`.
fn coding() -> Lossy {
    parse_any("coding").unwrap()
}
