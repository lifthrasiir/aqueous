//! Edge cases of FITS files: special floats, the other BITPIX, sizes the header lies about,
//! missing END, ASCII tables, heaps and padding, and damage to all of them: whatever `encode`
//! takes evaluates back to the input, and nothing panics, hangs or allocates what the data
//! cannot hold.

use aqr_archive::Evaluate;
use aqr_fits::formula::{self, IMAGE, RICE, TABLE};
use aqr_formula::{Apply, Formula, Module};
use aqr_testkit::Refusing;
use std::time::Duration;

#[global_allocator]
static A: Refusing = Refusing(256 << 20);

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        let r = match a.module {
            m if m == IMAGE => formula::decode_image(&a.params, inputs, a.len),
            m if m == RICE => formula::decode_rice(&a.params, inputs, a.len),
            m if m == TABLE => formula::decode_table(&a.params, inputs, a.len),
            m => panic!("{m}"),
        };
        r.map_err(|e| aqr_formula::Error::new(e.0))
    })
}

fn modules(f: &Formula) -> Vec<Module> {
    f.applies().iter().map(|a| a.module).collect()
}

fn card(s: &str) -> Vec<u8> {
    format!("{s:<80}").into_bytes()
}

fn header(cards: &[String]) -> Vec<u8> {
    let mut h: Vec<u8> = cards.iter().flat_map(|c| card(c)).collect();
    h.extend(card("END"));
    h.resize(h.len().next_multiple_of(2880), b' ');
    h
}

fn kv(k: &str, v: impl std::fmt::Display) -> String {
    format!("{k:<8}= {v:>20}")
}

fn pad(d: &mut Vec<u8>) {
    d.resize(d.len().next_multiple_of(2880), 0);
}

/// A primary HDU of a 2-D image of `bitpix` with `data`.
fn image(bitpix: i32, w: usize, h: usize, data: &[u8]) -> Vec<u8> {
    let mut d = header(&[kv("SIMPLE", "T"), kv("BITPIX", bitpix), kv("NAXIS", 2), kv("NAXIS1", w), kv("NAXIS2", h)]);
    d.extend(data);
    pad(&mut d);
    d
}

/// Round trips `d`, whatever `encode` makes of it.
fn round(d: &[u8], name: &str) -> Option<Formula> {
    let f = formula::encode(d, 1)?;
    assert!(eval(&f).unwrap() == d, "{name}");
    Some(f)
}

/// Noise of 0 to 19, the same each time.
fn noise(i: u32) -> u32 {
    (i.wrapping_mul(2654435761) >> 13) % 20
}

/// A smooth 16-bit image with noise, which the model codes smaller than zstd does.
fn smooth16(n: u32) -> Vec<u8> {
    (0..n).flat_map(|i| ((i % 50 * 7 + i / 50 * 3 + noise(i)) as i16).to_be_bytes()).collect()
}

/// A smooth float image whose pixels are, now and then, one of the special values.
fn specials32() -> Vec<u8> {
    specials(true)
}

fn specials(on: bool) -> Vec<u8> {
    let special = [
        f32::NAN.to_bits(),
        f32::INFINITY.to_bits(),
        f32::NEG_INFINITY.to_bits(),
        (-0.0f32).to_bits(),
        0x0000_0001, // the least subnormal
        0x807f_ffff, // the greatest negative subnormal
        0x7fc0_0001, // a NaN with a payload
        0xffff_ffff,
    ];
    let mut d = Vec::new();
    for i in 0..1600u32 {
        let v = if on && i % 200 == 3 {
            special[(i / 200) as usize % special.len()]
        } else {
            ((i % 40) as f32 * 7.0 + (i / 40) as f32 * 3.0 + noise(i) as f32).to_bits() & !0x3fff
        };
        d.extend(v.to_be_bytes());
    }
    d
}

#[test]
fn float_specials() {
    let plain = round(&image(-32, 40, 40, &specials(false)), "plain").unwrap();
    assert_eq!(modules(&plain), [IMAGE], "the image without specials is coded");
    // (the subnormals and a payload set the low bits every other pixel has zero: the model
    // then loses to zstd, and the image is kept as it is; the others are coded)
    round(&image(-32, 40, 40, &specials32()), "f32");
    let coarse: Vec<u8> = specials32()
        .chunks(4)
        .flat_map(|c| (u32::from_be_bytes(c.try_into().unwrap()) & !0x3fff).to_be_bytes())
        .collect();
    let f = round(&image(-32, 40, 40, &coarse), "coarse").unwrap();
    assert_eq!(modules(&f), [IMAGE], "NaN, infinities and negative zero are coded");
    // the same bits everywhere, each special
    for (name, bits) in
        [("nan", f32::NAN.to_bits()), ("-0", 0x8000_0000), ("subnormal", 1), ("inf", f32::INFINITY.to_bits())]
    {
        let data: Vec<u8> = (0..2000).flat_map(|_| bits.to_be_bytes()).collect();
        round(&image(-32, 50, 40, &data), name);
    }
    let data: Vec<u8> =
        (0..2000u32).flat_map(|i| (if i % 2 == 0 { 0.0f32 } else { -0.0 }).to_bits().to_be_bytes()).collect();
    round(&image(-32, 50, 40, &data), "alternating zeros");
}

/// BITPIX 64 and -64 are valid FITS but not coded: kept as they are.
#[test]
fn wide_pixels() {
    for bitpix in [64, -64] {
        let data: Vec<u8> = (0..1000u64).flat_map(|i| (i * 12345 + 1).to_be_bytes()).collect();
        let f = round(&image(bitpix, 40, 25, &data), "wide").unwrap();
        assert!(f.applies().is_empty(), "{bitpix}");
    }
    let nan: Vec<u8> = (0..1000).flat_map(|_| f64::NAN.to_bits().to_be_bytes()).collect();
    round(&image(-64, 40, 25, &nan), "f64 nan");
    round(&image(16, 0, 0, &[]), "empty");
}

#[test]
fn lying_sizes() {
    let data: Vec<u8> = (0..4000u32).flat_map(|i| (i as i16).to_be_bytes()).collect();
    let big = [i64::MAX, i64::MAX / 2 + 1, 1 << 62, 1 << 32, 1 << 31, i64::MAX / 8];
    for &w in &big {
        for (a, b) in [(w, 1), (w, 2), (w, 3), (2, w), (w, w)] {
            let mut d =
                header(&[kv("SIMPLE", "T"), kv("BITPIX", 16), kv("NAXIS", 2), kv("NAXIS1", a), kv("NAXIS2", b)]);
            d.extend(&data);
            pad(&mut d);
            round(&d, &format!("{a}x{b}"));
        }
    }
    // three axes whose product wraps to something small
    let mut d = header(&[
        kv("SIMPLE", "T"),
        kv("BITPIX", 16),
        kv("NAXIS", 3),
        kv("NAXIS1", 1u64 << 32),
        kv("NAXIS2", 1u64 << 32),
        kv("NAXIS3", 4),
    ]);
    d.extend(&data);
    pad(&mut d);
    round(&d, "wrapping product");
    // PCOUNT and GCOUNT huge or negative
    for (p, g) in [(i64::MAX, 1), (0, i64::MAX), (-1, 1), (0, -1), (1 << 62, 1 << 62)] {
        let mut d = header(&[
            kv("SIMPLE", "T"),
            kv("BITPIX", 16),
            kv("NAXIS", 2),
            kv("NAXIS1", 10),
            kv("NAXIS2", 10),
            kv("PCOUNT", p),
            kv("GCOUNT", g),
        ]);
        d.extend(&data);
        pad(&mut d);
        round(&d, &format!("{p} {g}"));
    }
    // 999 and 1000 axes, a negative count of them
    for n in [999, 1000, -1] {
        let mut c = vec![kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", n)];
        c.extend((1..=n.max(0)).map(|i| kv(&format!("NAXIS{i}"), 1)));
        let mut d = header(&c);
        d.extend([0u8; 100]);
        pad(&mut d);
        round(&d, "naxis");
    }
}

/// No END card: no HDU, and so no FITS file; END in a later block; END as the last card of the
/// first.
#[test]
fn missing_end() {
    let mut d = Vec::new();
    for c in [kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", 0)] {
        d.extend(card(&c));
    }
    d.resize(5760, b' ');
    assert!(formula::encode(&d, 1).is_none());
    assert!(formula::extent(&d).is_none());
    // the END in the second block, the first of blanks after the cards
    let mut d = Vec::new();
    for c in [kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", 0)] {
        d.extend(card(&c));
    }
    d.resize(2880, b' ');
    d.extend(card("END"));
    d.resize(5760, b' ');
    assert_eq!(formula::extent(&d), Some(5760));
    round(&d, "end late");
    // END the last card of the block
    let cards: Vec<String> = [kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", 0)]
        .into_iter()
        .chain((0..32).map(|i| format!("COMMENT {i}")))
        .collect();
    let d = header(&cards);
    assert_eq!(d.len(), 2880);
    assert_eq!(formula::extent(&d), Some(2880));
    // "ENDX" is not END
    let mut d = header(&[kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", 0)]);
    d[240..244].copy_from_slice(b"ENDX");
    assert!(formula::extent(&d).is_none());
}

/// An empty primary HDU, which every extension follows.
fn primary() -> Vec<u8> {
    header(&[kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", 0), kv("EXTEND", "T")])
}

fn ascii_table(rows: usize) -> Vec<u8> {
    let mut d = primary();
    d.extend(header(&[
        kv("XTENSION", "'TABLE   '"),
        kv("BITPIX", 8),
        kv("NAXIS", 2),
        kv("NAXIS1", 20),
        kv("NAXIS2", rows),
        kv("PCOUNT", 0),
        kv("GCOUNT", 1),
        kv("TFIELDS", 2),
        kv("TFORM1", "'I8      '"),
        kv("TBCOL1", 1),
        kv("TFORM2", "'F10.3   '"),
        kv("TBCOL2", 10),
    ]));
    for r in 0..rows {
        d.extend(format!("{:>8}  {:>10.3}", r * 7, r as f64 / 8.0).bytes());
    }
    pad(&mut d);
    d
}

/// A binary table of a `P` and a `Q` descriptor each row (24 bytes), a heap of
/// `heap` bytes at `theap` (default: right after the rows).
fn bintable_heap(rows: usize, theap: Option<usize>, heap: usize) -> Vec<u8> {
    bintable_theap(rows, theap, theap.map(|t| t as i64), heap)
}

/// As [`bintable_heap`], with a `THEAP` card of `card` (which the data need not follow).
fn bintable_theap(rows: usize, theap: Option<usize>, card: Option<i64>, heap: usize) -> Vec<u8> {
    let row = 24;
    let gap = theap.map_or(0, |t| t - rows * row);
    let mut c = vec![
        kv("XTENSION", "'BINTABLE'"),
        kv("BITPIX", 8),
        kv("NAXIS", 2),
        kv("NAXIS1", row),
        kv("NAXIS2", rows),
        kv("PCOUNT", gap + heap),
        kv("GCOUNT", 1),
        kv("TFIELDS", 2),
        kv("TFORM1", "'1PJ(8)  '"),
        kv("TFORM2", "'1QB(8)  '"),
    ];
    if let Some(t) = card {
        c.push(kv("THEAP", t));
    }
    let mut d = primary();
    d.extend(header(&c));
    for r in 0..rows {
        d.extend(((r % 3) as u32).to_be_bytes());
        d.extend(((r * 4) as u32).to_be_bytes());
        d.extend((r as u64 % 2).to_be_bytes());
        d.extend(((r * 3) as u64).to_be_bytes());
    }
    d.extend(vec![0xeeu8; gap]);
    d.extend((0..heap).map(|i| (i % 251) as u8));
    pad(&mut d);
    d
}

#[test]
fn tables() {
    let f = round(&ascii_table(300), "ascii").unwrap();
    assert!(f.applies().is_empty(), "ASCII tables are text, not columns");
    // variable-length arrays: a heap right after the rows, one after a gap (THEAP), no heap
    for (rows, theap, heap) in
        [(200, None, 500), (200, Some(200 * 24 + 40), 500), (200, None, 0), (200, Some(200 * 24), 7)]
    {
        let name = format!("{rows} {theap:?} {heap}");
        let f = round(&bintable_heap(rows, theap, heap), &name).unwrap();
        assert_eq!(modules(&f), [TABLE], "{name}");
    }
    // THEAP before the rows' end and far past the data (stated, not honoured)
    for t in [10, 1i64 << 40, -5] {
        round(&bintable_theap(200, None, Some(t), 100), &format!("theap {t}"));
    }
}

/// The padding's bytes are not zeros, a cut in or before the padding.
#[test]
fn padding() {
    let data = smooth16(1440);
    let d = image(16, 1440, 1, &data);
    assert_eq!(d.len(), 5760);
    round(&d, "no padding");
    for fill in [0x20u8, 0xff, 0x41] {
        let mut d = image(16, 1000, 1, &data[..2000]);
        let n = d.len();
        d[n - 880..].fill(fill);
        round(&d, "padding");
    }
    let d = image(16, 1000, 1, &data[..2000]);
    for cut in [d.len() - 1, d.len() - 880, 2880 + 2000, 2880 + 2001] {
        round(&d[..cut], "cut");
    }
    // a primary without data, then an image extension
    let mut d = primary();
    d.extend(header(&[
        kv("XTENSION", "'IMAGE   '"),
        kv("BITPIX", 16),
        kv("NAXIS", 2),
        kv("NAXIS1", 1100),
        kv("NAXIS2", 1),
        kv("PCOUNT", 0),
        kv("GCOUNT", 1),
    ]));
    d.extend(&data[..2200]);
    pad(&mut d);
    let f = round(&d, "extension").unwrap();
    assert_eq!(modules(&f), [IMAGE]);
}

/// Every kind of file above damaged: what is taken evaluates to the input.
#[test]
fn damaged_files() {
    let data = smooth16(1500);
    let files = [
        image(-32, 40, 40, &specials32()),
        ascii_table(100),
        bintable_heap(150, Some(150 * 24 + 8), 300),
        image(16, 50, 30, &data),
    ];
    for (i, d) in files.iter().enumerate() {
        aqr_testkit::damaged(d, 24, 200, Duration::from_secs(20), |v| {
            if let Some(f) = formula::encode(v, 1) {
                assert!(eval(&f).as_deref() == Ok(v), "file {i} does not round trip");
            }
            let _ = formula::extent(v);
        });
    }
}
