//! Edge cases: damaged files, huge dimensions, other color modes and depths, ZIP data, odd layer
//! counts, PSB with corrupt 64-bit lengths. Whatever the input, no panic, no hang, no allocation
//! it cannot hold; what is coded evaluates to the input.

mod common;

use aqr_archive::Evaluate;
use aqr_psd::formula;
use aqr_testkit::{Refusing, case, damaged, most, within};
use common::*;
use std::time::Duration;

#[global_allocator]
static A: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(60);
/// What no input of these tests holds a request of.
const SMALL: usize = 64 << 20;

fn modules(a: &aqr_formula::Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    formula::apply(a, inputs, 1)
}

/// Whether `file` is coded; if it is, it evaluates to itself.
fn roundtrip(file: &[u8]) -> bool {
    match formula::encode(file, 1) {
        Some(f) => {
            assert_eq!(f.eval(&modules).unwrap(), file, "roundtrip");
            true
        }
        None => false,
    }
}

struct Header<'a> {
    psb: bool,
    channels: u16,
    w: u32,
    h: u32,
    depth: u16,
    mode: u16,
    color_data: &'a [u8],
}

impl Header<'_> {
    fn rgb(w: u32, h: u32) -> Header<'static> {
        Header { psb: false, channels: 3, w, h, depth: 8, mode: 3, color_data: &[] }
    }

    /// A file of this header and of a composite image alone, `comp` of its compression (0 raw,
    /// 1 RLE, 2 and 3 ZIP stored in zlib), the planes given.
    fn file(&self, comp: u16, planes: &[u8]) -> Vec<u8> {
        let mut o = self.head(comp);
        let row = (self.w as usize * self.depth as usize).div_ceil(8).max(1);
        match comp {
            0 => o.extend_from_slice(planes),
            1 => o.extend_from_slice(&rle_counted(
                planes,
                row,
                aqr_psd::packbits::NO_TILES,
                0,
                if self.psb { 4 } else { 2 },
            )),
            _ => o.extend_from_slice(&zlib_stored(planes)),
        }
        o
    }

    /// Up to the composite's compression field.
    fn head(&self, comp: u16) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(b"8BPS");
        be16(&mut o, if self.psb { 2 } else { 1 });
        o.extend_from_slice(&[0; 6]);
        be16(&mut o, self.channels);
        be32(&mut o, self.h);
        be32(&mut o, self.w);
        be16(&mut o, self.depth);
        be16(&mut o, self.mode);
        be32(&mut o, self.color_data.len() as u32);
        o.extend_from_slice(self.color_data);
        be32(&mut o, 0);
        if self.psb {
            o.extend_from_slice(&[0; 8]);
        } else {
            be32(&mut o, 0);
        }
        be16(&mut o, comp);
        o
    }
}

/// Stored deflate blocks in a zlib wrapper.
fn zlib_stored(d: &[u8]) -> Vec<u8> {
    let mut o = vec![0x78, 0x01];
    if d.is_empty() {
        o.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    let n = d.chunks(65535).count();
    for (i, c) in d.chunks(65535).enumerate() {
        o.push((i + 1 == n) as u8);
        o.extend_from_slice(&(c.len() as u16).to_le_bytes());
        o.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        o.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    o.extend_from_slice(&((b << 16) | a).to_be_bytes());
    o
}

/// Planes of `n` channels of `w` x `h` samples of `depth` bits, smooth with some noise.
fn planes(n: usize, w: usize, h: usize, depth: usize) -> Vec<u8> {
    let row = (w * depth).div_ceil(8);
    let mut o = Vec::new();
    for c in 0..n {
        o.extend(image(row, h, c as u32 * 7));
    }
    o
}

/// Delta coding along the rows as ZIP with prediction has it, for 8-bit samples.
fn predicted(d: &[u8], row: usize) -> Vec<u8> {
    let mut o = d.to_vec();
    for r in o.chunks_mut(row) {
        for i in (1..r.len()).rev() {
            r[i] = r[i].wrapping_sub(r[i - 1]);
        }
    }
    o
}

fn small_layered() -> Vec<u8> {
    hinted_file(60, 10)
}

#[test]
fn mutated_source_files() {
    let file = small_layered();
    assert!(roundtrip(&file));
    damaged(&file, 60, file.len().min(400), LIMIT, |v| {
        let _ = aqr_psd::parse(v);
        let _ = formula::regions(v);
        roundtrip(v);
        assert!(most() < SMALL, "an allocation of {}", most());
    });
}

#[test]
fn mutated_psb() {
    let file = psb(40, 8, aqr_psd::packbits::NO_TILES);
    assert!(roundtrip(&file));
    damaged(&file, 40, file.len().min(300), LIMIT, |v| {
        roundtrip(v);
        assert!(most() < SMALL, "an allocation of {}", most());
    });
}

#[test]
fn truncated_at_every_offset() {
    let file = small_layered();
    for n in 0..file.len() {
        let v = &file[..n];
        case(&format!("cut at {n}"));
        within(LIMIT, || roundtrip(v)).unwrap();
        assert!(most() < SMALL);
    }
}

#[test]
fn huge_dimensions_and_channel_counts() {
    let row = planes(1, 16, 4, 8);
    for (psb, w, h, channels) in [
        (false, 30000u32, 30000u32, 56u16),
        (true, 300000, 300000, 56),
        (true, u32::MAX, u32::MAX, u16::MAX),
        (false, u32::MAX, 1, 3),
        (false, 1, u32::MAX, 3),
        (false, 0, 0, 3),
        (false, 16, 4, 0),
    ] {
        for comp in [0, 1, 2, 3] {
            for depth in [1, 8, 16, 32] {
                let header = Header { psb, channels, w, h, depth, mode: 3, color_data: &[] };
                let file = header.file(comp, &row);
                case(&format!("{w}x{h} {channels} channels depth {depth} comp {comp} psb {psb}"));
                let coded = within(LIMIT, || roundtrip(&file)).unwrap();
                assert!(!coded);
                assert!(most() < SMALL, "an allocation of {}", most());
            }
        }
    }
}

#[test]
fn color_modes_and_depths() {
    let palette = vec![7u8; 768];
    // (mode, channels, depth, color mode data)
    let cases: &[(u16, u16, u16, &[u8])] = &[
        (0, 1, 1, &[]),
        (1, 1, 8, &[]),
        (1, 1, 16, &[]),
        (1, 2, 8, &[]),
        (2, 1, 8, &palette),
        (3, 3, 8, &[]),
        (3, 3, 16, &[]),
        (3, 4, 32, &[]),
        (4, 4, 8, &[]),
        (4, 5, 16, &[]),
        (4, 4, 32, &[]),
        (7, 3, 8, &[]),
        (8, 2, 8, &[]),
        (9, 3, 16, &[]),
    ];
    let (w, h) = (96usize, 24usize);
    for &(mode, channels, depth, color_data) in cases {
        let pl = planes(channels as usize, w, h, depth as usize);
        for comp in [0u16, 1, 2, 3] {
            let data = if comp == 3 { predicted(&pl, (w * depth as usize).div_ceil(8)) } else { pl.clone() };
            for psb in [false, true] {
                let header = Header { psb, channels, w: w as u32, h: h as u32, depth, mode, color_data };
                let file = header.file(comp, &data);
                case(&format!("mode {mode} {channels} channels depth {depth} comp {comp} psb {psb}"));
                within(LIMIT, || roundtrip(&file)).unwrap();
            }
        }
    }
}

#[test]
fn composites_of_8_bit_modes_are_coded() {
    let (w, h) = (200usize, 40usize);
    for (mode, channels, color_data) in [(1u16, 1u16, vec![]), (4, 4, vec![]), (2, 1, vec![0u8; 768])] {
        let pl = planes(channels as usize, w, h, 8);
        let header = Header { psb: false, channels, w: w as u32, h: h as u32, depth: 8, mode, color_data: &color_data };
        for comp in [0, 1] {
            assert!(roundtrip(&header.file(comp, &pl)), "mode {mode} comp {comp} not coded");
        }
    }
}

#[test]
fn zip_with_wrong_lengths_or_checksums() {
    let (w, h) = (64usize, 16usize);
    let pl = planes(3, w, h, 8);
    let header = Header::rgb(w as u32, h as u32);
    for comp in [2u16, 3] {
        let ok = header.file(comp, &pl);
        within(LIMIT, || roundtrip(&ok)).unwrap();
        // short, long, garbage and empty inflated data
        let long = [pl.clone(), vec![0]].concat();
        for data in [&pl[..pl.len() - 1], &long[..], &[0u8; 50][..], &[][..]] {
            let f = header.file(comp, data);
            case("zip wrong length");
            within(LIMIT, || roundtrip(&f)).unwrap();
        }
        let mut f = ok.clone();
        let n = f.len();
        f[n - 1] ^= 1; // the Adler-32
        within(LIMIT, || roundtrip(&f)).unwrap();
        within(LIMIT, || roundtrip(&ok[..n - 7])).unwrap();
        // a stored block claiming more than there is
        let mut f = header.head(comp);
        f.extend_from_slice(&[0x78, 0x01, 0x01, 0xff, 0xff, 0, 0]);
        within(LIMIT, || roundtrip(&f)).unwrap();
    }
}

/// `file` with the layer count (a signed 16-bit field) set.
fn with_layer_count(file: &[u8], count: i16) -> Vec<u8> {
    let p = aqr_psd::parse(file).unwrap();
    let at = p.layer_info.span.start;
    let mut v = file.to_vec();
    v[at..at + 2].copy_from_slice(&count.to_be_bytes());
    v
}

#[test]
fn layer_counts_zero_negative_and_wrong() {
    let file = psd(120, 20, &layers(120, 20, true), aqr_psd::packbits::NO_TILES, 0, &[]);
    assert!(roundtrip(&file));
    for count in [0i16, -1, -2, 1, 3, 100, i16::MAX, i16::MIN] {
        let v = with_layer_count(&file, count);
        case(&format!("layer count {count}"));
        within(LIMIT, || roundtrip(&v)).unwrap();
        assert!(most() < SMALL);
    }
    // no layers at all: the composite alone
    assert!(roundtrip(&psd(120, 20, &[], aqr_psd::packbits::NO_TILES, 0, &[])));
}

/// `file` with its layer info moved into a global block `key` (as 16- and 32-bit files have
/// it: `Lr16`, `Lr32`), the layer info proper empty.
fn in_block(file: &[u8], key: &[u8; 4], psb: bool) -> Vec<u8> {
    let w = if psb { 8 } else { 4 };
    let p = aqr_psd::parse(file).unwrap();
    let data = &file[p.layer_info.span.start..p.layer_info.span.end];
    let mut inner = vec![0; w]; // the layer info: empty
    be32(&mut inner, 0); // global mask
    inner.extend_from_slice(b"8BIM");
    inner.extend_from_slice(key);
    if psb {
        inner.extend_from_slice(&(data.len() as u64).to_be_bytes());
    } else {
        be32(&mut inner, data.len() as u32);
    }
    inner.extend_from_slice(data);
    let mut o = file[..p.layer_section.start].to_vec();
    if psb {
        o.extend_from_slice(&(inner.len() as u64).to_be_bytes());
    } else {
        be32(&mut o, inner.len() as u32);
    }
    o.extend_from_slice(&inner);
    o.extend_from_slice(&file[p.composite.start..]);
    o
}

fn layered(w: usize, h: usize) -> Vec<u8> {
    psd(w, h, &layers(w, h, true), aqr_psd::packbits::NO_TILES, 0, &[])
}

#[test]
fn lr16_and_lr32_blocks() {
    let file = layered(80, 12);
    for key in [b"Lr16", b"Lr32", b"Layr"] {
        let v = in_block(&file, key, false);
        case(&String::from_utf8_lossy(key));
        let p = aqr_psd::parse(&v).expect("parses");
        assert_eq!(p.layer_info.layers.len(), 2, "layers of the global block");
        within(LIMIT, || roundtrip(&v)).unwrap();
        damaged(&v, 30, 200, LIMIT, |d| {
            roundtrip(d);
            assert!(most() < SMALL);
        });
    }
}

#[test]
fn psb_with_corrupt_64_bit_lengths() {
    let file = psb(40, 8, aqr_psd::packbits::NO_TILES);
    let p = aqr_psd::parse(&file).unwrap();
    // the layer and mask section's length is the 8 bytes before its start
    let at = p.layer_section.start;
    for len in [u64::MAX, 1 << 63, 1 << 40, (file.len() as u64) + 1, 1, 7] {
        let mut v = file.clone();
        v[at..at + 8].copy_from_slice(&len.to_be_bytes());
        case(&format!("psb layer section {len}"));
        within(LIMIT, || roundtrip(&v)).unwrap();
        assert!(most() < SMALL);
    }
    // a PSB holding layers in a block, the lengths of the layer info and of the block
    let file = in_block(&psb_layered(80, 12), b"Lr16", true);
    within(LIMIT, || roundtrip(&file)).unwrap();
    let p = aqr_psd::parse(&file).unwrap();
    let section = p.layer_section.start;
    let tag = file.windows(8).position(|w| w == b"8BIMLr16").unwrap() + 8;
    for (name, at) in [("section", section), ("layer info", section + 8), ("block", tag)] {
        for len in [u64::MAX, 1 << 63, 1 << 40, 3] {
            let mut v = file.clone();
            v[at..at + 8].copy_from_slice(&len.to_be_bytes());
            case(&format!("psb {name} {len}"));
            within(LIMIT, || roundtrip(&v)).unwrap();
            assert!(most() < SMALL);
        }
    }
}

/// A PSB of one raw layer of 3 channels (8-byte channel lengths) and a composite.
fn psb_layered(w: usize, h: usize) -> Vec<u8> {
    let mut info = Vec::new();
    be16(&mut info, 1);
    for v in [0u32, 0, h as u32, w as u32] {
        be32(&mut info, v);
    }
    be16(&mut info, 3);
    for id in 0..3u16 {
        be16(&mut info, id);
        info.extend_from_slice(&((2 + w * h) as u64).to_be_bytes());
    }
    info.extend_from_slice(b"8BIMnorm");
    info.extend_from_slice(&[255, 0, 8, 0]);
    let mut extra = Vec::new();
    be32(&mut extra, 0); // mask
    be32(&mut extra, 0); // blending ranges
    extra.extend_from_slice(&[3, b'a', b'b', b'c']);
    be32(&mut info, extra.len() as u32);
    info.extend_from_slice(&extra);
    for k in 0..3 {
        be16(&mut info, 0);
        info.extend(image(w, h, k));
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut lm = Vec::new();
    lm.extend_from_slice(&(info.len() as u64).to_be_bytes());
    lm.extend_from_slice(&info);
    be32(&mut lm, 0); // global mask
    let mut o = Header { psb: true, ..Header::rgb(w as u32, h as u32) }.head(1);
    o.truncate(o.len() - 2 - 8); // up to the layer and mask section's length
    o.extend_from_slice(&(lm.len() as u64).to_be_bytes());
    o.extend_from_slice(&lm);
    be16(&mut o, 1);
    let planes: Vec<u8> = (0..3).flat_map(|k| image(w, h, 77 + k)).collect();
    o.extend_from_slice(&rle_counted(&planes, w, aqr_psd::packbits::NO_TILES, 0, 4));
    o
}

#[test]
fn mutated_psb_with_layers() {
    let file = psb_layered(24, 6);
    assert!(aqr_psd::parse(&file).is_ok());
    damaged(&file, 40, file.len().min(250), LIMIT, |v| {
        roundtrip(v);
        assert!(most() < SMALL, "an allocation of {}", most());
    });
}

#[test]
fn layers_of_absurd_rectangles_and_canvases() {
    let file = hinted_file(60, 10);
    let p = aqr_psd::parse(&file).unwrap();
    // the first layer's rectangle (top, left, bottom, right), after the count
    let rect = p.layer_info.span.start + 2;
    for (field, value) in [(1usize, i32::MIN), (3, i32::MAX), (0, i32::MIN), (2, i32::MAX), (1, -1 << 24)] {
        let mut v = file.clone();
        v[rect + 4 * field..][..4].copy_from_slice(&value.to_be_bytes());
        case(&format!("rect field {field} = {value}"));
        within(LIMIT, || roundtrip(&v)).unwrap();
        assert!(most() < SMALL, "an allocation of {}", most());
    }
    // the canvas (height at 14, width at 18): layers and a composite over it
    for (at, value) in [(14usize, u32::MAX), (18, u32::MAX), (18, 300000), (14, 0), (18, 0)] {
        let mut v = file.clone();
        v[at..at + 4].copy_from_slice(&value.to_be_bytes());
        case(&format!("canvas field at {at} = {value}"));
        within(LIMIT, || roundtrip(&v)).unwrap();
        assert!(most() < SMALL, "an allocation of {}", most());
    }
}

/// Row-wise delta of ZIP with prediction at `depth` (the inverse of `pixels`'s): 8 bits are
/// bytes, 16 bits are big-endian words, 32 bits are split into byte planes in the row, then
/// bytes are delta coded across the row.
fn predicted_at(d: &[u8], width: usize, depth: usize) -> Vec<u8> {
    let rb = width * depth / 8;
    let mut o = d.to_vec();
    for row in o.chunks_mut(rb) {
        match depth {
            8 => (1..rb).rev().for_each(|i| row[i] = row[i].wrapping_sub(row[i - 1])),
            16 => {
                for i in (1..width).rev() {
                    let v = u16::from_be_bytes([row[2 * i], row[2 * i + 1]])
                        .wrapping_sub(u16::from_be_bytes([row[2 * i - 2], row[2 * i - 1]]));
                    row[2 * i..2 * i + 2].copy_from_slice(&v.to_be_bytes());
                }
            }
            _ => {
                let inter = row.to_vec();
                for x in 0..width {
                    for b in 0..4 {
                        row[b * width + x] = inter[4 * x + b];
                    }
                }
                (1..rb).rev().for_each(|i| row[i] = row[i].wrapping_sub(row[i - 1]));
            }
        }
    }
    o
}

/// The composite image is decoded to its planes whatever the compression and depth.
#[test]
fn composite_planes_are_decoded() {
    let (w, h) = (13usize, 6usize);
    for depth in [8usize, 16, 32] {
        let pl = planes(3, w, h, depth);
        let header =
            Header { psb: false, channels: 3, w: w as u32, h: h as u32, depth: depth as u16, mode: 3, color_data: &[] };
        let sizes = [(0u16, pl.clone()), (2, pl.clone()), (3, predicted_at(&pl, w, depth))];
        let mut files: Vec<(u16, Vec<u8>)> = sizes.iter().map(|(c, d)| (*c, header.file(*c, d))).collect();
        if depth == 8 {
            // (RLE of 8-bit planes: rows of w bytes)
            files.push((1, header.file(1, &pl)));
            let psb = Header { psb: true, ..header };
            files.push((1, psb.file(1, &pl)));
        }
        for (comp, file) in files {
            let p = aqr_psd::parse(&file).unwrap();
            let got =
                aqr_psd::pixels::composite(&p, &file).unwrap_or_else(|e| panic!("depth {depth} comp {comp}: {e}"));
            assert_eq!(got, pl, "depth {depth} comp {comp}");
        }
    }
    assert_eq!(
        [0u16, 1, 2, 3, 9].map(|c| aqr_psd::psd::Compression::from_u16(c).name()),
        ["raw", "rle", "zip", "zippred", "comp9"]
    );
}

/// The channels of layers are decoded by their own compression; a channel of no data is empty.
#[test]
fn layer_channels_are_decoded() {
    let (w, h) = (20usize, 7usize);
    let ls = layers(w, h, true);
    let file = psd(w, h, &ls, aqr_psd::packbits::NO_TILES, 0, &[1]);
    let p = aqr_psd::parse(&file).unwrap();
    for (i, l) in ls.iter().enumerate() {
        assert_eq!(p.layer_info.layers[i].channels.len(), l.channels.len());
        for (j, (_, data)) in l.channels.iter().enumerate() {
            let got = aqr_psd::pixels::layer_channel(&p, &file, i, j).unwrap();
            assert_eq!(&got, data, "layer {i} channel {j} ({})", if i == 1 { "raw" } else { "RLE" });
        }
    }
    // an unknown compression of a channel
    let mut cut = file.clone();
    let c = &p.layer_info.layers[0].channels[0];
    cut[c.span.start + 1] = 9; // an unknown compression
    let p = aqr_psd::parse(&cut).unwrap();
    let e = aqr_psd::pixels::layer_channel(&p, &cut, 0, 0).unwrap_err();
    assert!(e.to_string().contains("compression 9"), "{e}");
}

/// Data that does not hold its planes is an error, not a panic or a short plane.
#[test]
fn damaged_composite_planes_are_errors() {
    let (w, h) = (16usize, 4usize);
    let pl = planes(3, w, h, 8);
    let header = Header::rgb(w as u32, h as u32);
    let decode = |file: &[u8]| {
        let p = aqr_psd::parse(file).ok()?;
        Some(aqr_psd::pixels::composite(&p, file).map_err(|e| e.to_string()))
    };
    let err = |file: &[u8], what: &str| {
        let r = decode(file).unwrap_or_else(|| panic!("{what}: not parsed"));
        assert!(r.is_err(), "{what}: decoded");
        r.unwrap_err()
    };
    assert!(err(&header.file(0, &pl[..pl.len() - 1]), "raw short").contains("raw data too short"));
    // RLE: counts cut, a row cut, rows decoding to the wrong width, no data at all
    let rle = header.file(1, &pl);
    let counts = 2 * h * 3;
    let head = rle.len() - rle_counted(&pl, w, aqr_psd::packbits::NO_TILES, 0, 2).len();
    assert!(err(&rle[..head + counts - 1], "counts cut").contains("RLE counts truncated"));
    assert!(err(&rle[..rle.len() - 1], "last row cut").contains("RLE row truncated"));
    let mut wrong = header.head(1);
    for _ in 0..h * 3 {
        be16(&mut wrong, 2);
    }
    for _ in 0..h * 3 {
        wrong.extend_from_slice(&[0, 7]); // a literal of one byte, not 16
    }
    assert!(err(&wrong, "short rows").contains("RLE row length mismatch"));
    let mut bad = header.head(1);
    for _ in 0..h * 3 {
        be16(&mut bad, 2);
    }
    for _ in 0..h * 3 {
        bad.extend_from_slice(&[0x80, 0]); // -128: a no-op, so the row is empty
    }
    assert!(err(&bad, "empty rows").contains("RLE row"));
    // ZIP: a stream too short for its header, wrong inflated size, damaged deflate
    assert!(err(&[header.head(2), vec![0x78]].concat(), "one byte").contains("short zlib stream"));
    assert!(err(&header.file(2, &pl[..pl.len() - 1]), "zip short").contains("ZIP data size mismatch"));
    assert!(err(&header.file(3, &[pl.clone(), vec![0]].concat()), "zip long").contains("ZIP data size mismatch"));
    let mut z = header.head(2);
    z.extend_from_slice(&[0x78, 0x01, 0x07, 0x00, 0x00, 0x00]); // block type 3
    assert!(err(&z, "bad deflate").contains("zip channel"));
    assert!(err(&header.file(9, &pl), "unknown").contains("compression 9"));
}
