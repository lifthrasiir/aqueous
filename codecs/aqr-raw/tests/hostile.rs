//! Containers that lie: TIFF files whose IFDs loop or point at their parents, with counts and
//! strips past the end; WAV and AIFF chunks out of order, doubled, of sizes 0 and 0xFFFFFFFF;
//! BMP of OS/2 and V4/V5 headers and negative heights; DICOM and PAM and PNM headers malformed;
//! and all of them damaged. Taken or not, never a panic, a hang or an allocation the data
//! cannot hold; what is taken evaluates back.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};
use aqr_raw::container;
use aqr_raw::formula::{self, pcm, raster, raster16};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 28);

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        let r = match a.module {
            m if m == pcm::MODULE => {
                let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
                pcm::decode(&a.params, &res, inputs, a.len, 1)
            }
            m if m == raster::MODULE => raster::decode(&a.params, inputs, a.len, 1),
            m if m == raster16::MODULE => raster16::decode(&a.params, inputs, a.len),
            m if m == aqr_ima::formula::MODULE => {
                aqr_ima::formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_raw::module::Error(e.0.to_string()))
            }
            m if m == aqr_msadpcm::formula::MODULE => aqr_msadpcm::formula::decode(&a.params, inputs, a.len)
                .map_err(|e| aqr_raw::module::Error(e.0.to_string())),
            m => panic!("{m}"),
        };
        r.map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// What `detect` finds is within the file and evaluates back to it; and its extent too.
fn check(name: &str, d: &[u8]) -> Option<container::Found> {
    if let Some(e) = container::tiff_extent(d) {
        assert!(e <= d.len(), "{name}: extent {e}");
    }
    let found = container::detect(d)?;
    assert!(found.start.checked_add(found.len).is_some_and(|e| e <= d.len()), "{name}: {found:?}");
    if let Some(f) = formula::encode(d, 1) {
        assert_eq!(f.len(), d.len() as u64, "{name}");
        assert!(eval(&f).unwrap() == d, "{name}");
    }
    Some(found)
}

fn put16(d: &mut Vec<u8>, v: u16) {
    d.extend(v.to_le_bytes());
}

fn put32(d: &mut Vec<u8>, v: u32) {
    d.extend(v.to_le_bytes());
}

/// The entries of an IFD: tag, type, count and value.
type Entries = Vec<(u16, u16, u32, u32)>;

/// A little-endian TIFF: the IFDs as (entries (tag, type, count, value), next), laid out one
/// after another from offset 8, and `tail` after them.
fn tiff(ifds: &[(Entries, u32)], tail: &[u8]) -> Vec<u8> {
    let mut d = b"II*\0".to_vec();
    put32(&mut d, 8);
    for (entries, next) in ifds {
        put16(&mut d, entries.len() as u16);
        for &(tag, ty, count, value) in entries {
            put16(&mut d, tag);
            put16(&mut d, ty);
            put32(&mut d, count);
            put32(&mut d, value);
        }
        put32(&mut d, *next);
    }
    d.extend(tail);
    d
}

/// The entries of a good 4x4 grey 8-bit image whose strip is at `at`.
fn image_entries(at: u32) -> Entries {
    vec![
        (256, 3, 1, 4),
        (257, 3, 1, 4),
        (258, 3, 1, 8),
        (259, 3, 1, 1),
        (273, 4, 1, at),
        (277, 3, 1, 1),
        (279, 4, 1, 16),
    ]
}

fn good_tiff() -> Vec<u8> {
    let ifd = 2 + 12 * 7 + 4;
    let mut d = tiff(&[(image_entries(8 + ifd as u32), 0)], &[]);
    d.extend((0..16).map(|i| (i * 9) as u8));
    d
}

#[test]
fn good_is_taken() {
    let d = good_tiff();
    let f = check("good", &d).unwrap();
    assert_eq!(f.name, "tiff");
    assert_eq!(container::tiff_extent(&d), Some(d.len()));
}

#[test]
fn tiff_loops() {
    let strip = vec![7u8; 16];
    let one = 2 + 12 * 7 + 4;
    // an IFD whose next is itself, and a chain of two looping
    let d = tiff(&[(image_entries(8 + one as u32), 8)], &strip);
    assert!(container::tiff_extent(&d).is_some());
    check("self loop", &d);
    let d =
        tiff(&[(image_entries(8 + 2 * one as u32), 8 + one as u32), (image_entries(8 + 2 * one as u32), 8)], &strip);
    check("two loop", &d);
    // a SubIFD that is its parent (a back pointer), also as the Exif and GPS IFDs
    for tag in [330u16, 34665, 34853, 40965] {
        let mut e = image_entries(8 + 14 + 12 * 8_u32);
        e.push((tag, 4, 1, 8));
        let d = tiff(&[(e, 0)], &strip);
        check(&format!("back pointer {tag}"), &d);
    }
    // the next IFD past the end, in the header, or inside the entries
    for next in [u32::MAX, 0, 4, 10, 8 + 7] {
        let d = tiff(&[(image_entries(8 + one as u32), next)], &strip);
        check(&format!("next {next}"), &d);
    }
    // the first IFD past the end or in the header
    for at in [0u32, 2, 4, u32::MAX, 1 << 20] {
        let mut d = good_tiff();
        d[4..8].copy_from_slice(&at.to_le_bytes());
        check(&format!("ifd at {at}"), &d);
    }
    // a chain of 3000 IFDs (the extent follows a bounded number)
    let ifds: Vec<_> = (0..3000u32).map(|i| (vec![(256u16, 3u16, 1u32, 4u32)], 8 + (i + 1) * 18)).collect();
    let d = tiff(&ifds, &strip);
    check("chain", &d);
}

#[test]
fn tiff_counts_and_strips_past_the_end() {
    let one = 2 + 12 * 7 + 4;
    let at = 8 + one as u32;
    let strip = vec![7u8; 16];
    // strip offsets and counts, as values of the file or not: past the end, to the largest, a
    // count of values past the file, values at the end of the address space
    let variants: Vec<Entries> = vec![
        image_entries(1 << 30),
        image_entries(u32::MAX),
        image_entries(at - 1),
        {
            let mut e = image_entries(at);
            e[6].3 = u32::MAX;
            e
        },
        {
            let mut e = image_entries(at);
            e[4] = (273, 4, u32::MAX, at);
            e
        },
        {
            let mut e = image_entries(at);
            e[4] = (273, 4, u32::MAX, u32::MAX);
            e[6] = (279, 4, u32::MAX, u32::MAX);
            e
        },
        {
            let mut e = image_entries(at);
            e[4] = (273, 4, 2, at);
            e[6] = (279, 4, 2, at);
            e
        },
        {
            let mut e = image_entries(at);
            e.push((324, 4, u32::MAX, at));
            e.push((325, 4, u32::MAX, at));
            e.push((513, 4, 1, u32::MAX));
            e.push((514, 4, 1, u32::MAX));
            e.push((288, 4, 1, 4));
            e.push((289, 4, 1, u32::MAX));
            e
        },
        {
            let mut e = image_entries(at);
            e[0] = (256, 3, u32::MAX, 4);
            e
        },
    ];
    for (i, e) in variants.into_iter().enumerate() {
        let n = e.len() as u16;
        let mut d = tiff(&[(e, 0)], &strip);
        // (the entry count of the IFD as written)
        d[8..10].copy_from_slice(&n.to_le_bytes());
        check(&format!("variant {i}"), &d);
    }
    // an entry count past the file
    let mut d = good_tiff();
    d[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    check("entries", &d);
    // two strips, in order, and out of order, and of sizes whose sum is the raster's
    let mut e = image_entries(at);
    e[4] = (273, 4, 2, at + 16);
    e[6] = (279, 4, 2, at + 24);
    let mut d = tiff(&[(e, 0)], &[]);
    d.extend(0u32.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    check("strips", &d);
}

/// IFDs of a SubIFDs entry (tag 330) of a million offsets, each IFD again one: the values are
/// listed for each, and must not be kept as the IFDs to visit.
#[test]
fn tiff_subifds_of_many() {
    const N: u32 = 1_000_000;
    const K: u32 = 1100;
    let ifd = 2 + 12 + 4;
    let arr = 8 + K * ifd as u32 + 100;
    let ifds: Vec<_> = (0..K).map(|_| (vec![(330u16, 4u16, N, arr)], 0u32)).collect();
    let mut d = tiff(&ifds, &[]);
    d.resize(arr as usize, 0);
    for i in 0..N {
        put32(&mut d, 8 + (i % K) * ifd as u32);
    }
    let t = std::time::Instant::now();
    aqr_testkit::case("subifds of many");
    let e = aqr_testkit::within(Duration::from_secs(30), || container::tiff_extent(&d)).unwrap();
    assert!(e.is_some_and(|e| e <= d.len()));
    eprintln!("{:?}", t.elapsed());
}

fn wav_chunk(id: &[u8; 4], len: u32, body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(len.to_le_bytes());
    v.extend(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn fmt16(channels: u16, bits: u16) -> Vec<u8> {
    let mut v = vec![];
    for x in [1u16, channels] {
        v.extend(x.to_le_bytes());
    }
    v.extend(8000u32.to_le_bytes());
    v.extend((8000 * (bits as u32 / 8)).wrapping_mul(channels as u32).to_le_bytes());
    v.extend(channels.wrapping_mul(bits / 8).to_le_bytes());
    v.extend(bits.to_le_bytes());
    v
}

fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
    for c in chunks {
        v.extend(c);
    }
    let n = (v.len() - 8) as u32;
    v[4..8].copy_from_slice(&n.to_le_bytes());
    v
}

#[test]
fn wav_chunks() {
    let samples: Vec<u8> = (0..400).map(|i| (i * 3 + i / 7) as u8).collect();
    let fmt = wav_chunk(b"fmt ", 16, &fmt16(2, 16));
    let data = wav_chunk(b"data", samples.len() as u32, &samples);
    let good = riff(&[fmt.clone(), data.clone()]);
    let f = check("good", &good).unwrap();
    assert_eq!((f.name, f.len), ("wav", 400));
    // other chunks between, an odd one padded
    let odd = wav_chunk(b"junk", 5, b"12345");
    let f = check("between", &riff(&[odd.clone(), fmt.clone(), odd.clone(), data.clone()])).unwrap();
    assert_eq!(f.len, 400);
    // data before fmt: not taken (the layout is unknown); fmt after the data chunk is not seen
    assert!(check("data first", &riff(&[data.clone(), fmt.clone()])).is_none());
    // two data chunks: the first, the second left in the trailer
    let two = riff(&[fmt.clone(), data.clone(), wav_chunk(b"data", 4, b"abcd")]);
    assert_eq!(check("two data", &two).unwrap().len, 400);
    // two fmt chunks: the last before the data counts
    let f = check("two fmt", &riff(&[wav_chunk(b"fmt ", 16, &fmt16(1, 8)), fmt.clone(), data.clone()])).unwrap();
    assert_eq!(f.len, 400);
    // data sizes: 0 and 0xFFFFFFFF are the rest of the file, others clamped, odd sizes cut to
    // frames of 4 bytes
    for (len, want) in
        [(0u32, 400usize), (u32::MAX, 400), (1, 0), (3, 0), (4, 4), (402, 400), (1 << 30, 400), (398, 396)]
    {
        let d = riff(&[fmt.clone(), wav_chunk(b"data", len, &samples)]);
        let f = check(&format!("len {len}"), &d);
        assert_eq!(f.map_or(0, |f| f.len), want, "len {len}");
    }
    // a chunk of size 0xFFFFFFFF before the data, of the largest odd size, of size 0
    for len in [u32::MAX, u32::MAX - 1, 0x7fff_ffff, 0] {
        let d = riff(&[wav_chunk(b"junk", len, b"xx"), fmt.clone(), data.clone()]);
        check(&format!("junk {len}"), &d);
    }
    // the fmt chunk short, with a size past the file, with channels 0, with an align that is not
    // of the channels, with bits past the align, with a tag unknown
    for (name, f) in [
        ("short", wav_chunk(b"fmt ", 15, &fmt16(2, 16)[..15])),
        ("past", wav_chunk(b"fmt ", u32::MAX, &fmt16(2, 16))),
        ("no channels", wav_chunk(b"fmt ", 16, &fmt16(0, 16))),
        ("align", {
            let mut v = fmt16(2, 16);
            v[12] = 5;
            wav_chunk(b"fmt ", 16, &v)
        }),
        ("bits", {
            let mut v = fmt16(2, 16);
            v[14] = 64;
            wav_chunk(b"fmt ", 16, &v)
        }),
        ("bits 0", {
            let mut v = fmt16(2, 16);
            v[14] = 0;
            wav_chunk(b"fmt ", 16, &v)
        }),
        ("tag", {
            let mut v = fmt16(2, 16);
            v[0] = 85;
            wav_chunk(b"fmt ", 16, &v)
        }),
        ("many channels", wav_chunk(b"fmt ", 16, &fmt16(60000, 16))),
        ("extensible short", {
            let mut v = fmt16(2, 16);
            v[0] = 0xfe;
            v[1] = 0xff;
            wav_chunk(b"fmt ", 16, &v)
        }),
    ] {
        assert!(check(name, &riff(&[f, data.clone()])).is_none(), "{name}");
    }
    // cut at every place
    for n in 0..good.len() {
        check("cut", &good[..n]);
    }
}

fn comm(channels: u16, bits: u16) -> Vec<u8> {
    let mut v = b"COMM".to_vec();
    v.extend(18u32.to_be_bytes());
    v.extend(channels.to_be_bytes());
    v.extend(100u32.to_be_bytes());
    v.extend(bits.to_be_bytes());
    v.extend([0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    v
}

fn ssnd(len: u32, offset: u32, data: &[u8]) -> Vec<u8> {
    let mut v = b"SSND".to_vec();
    v.extend(len.to_be_bytes());
    v.extend(offset.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(data);
    v
}

fn form(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut v = b"FORM\0\0\0\0".to_vec();
    v.extend(kind);
    for c in chunks {
        v.extend(c);
    }
    v
}

#[test]
fn aiff_chunks() {
    let samples: Vec<u8> = (0..400).map(|i| (i * 3 + i / 7) as u8).collect();
    let good = form(b"AIFF", &[comm(2, 16), ssnd(408, 0, &samples)]);
    let f = check("good", &good).unwrap();
    assert_eq!((f.name, f.len), ("aiff", 400));
    // SSND before COMM: not taken
    assert!(check("ssnd first", &form(b"AIFF", &[ssnd(408, 0, &samples), comm(2, 16)])).is_none());
    // two SSND: the first
    let two = form(b"AIFF", &[comm(2, 16), ssnd(408, 0, &samples), ssnd(8, 0, &[])]);
    assert_eq!(check("two", &two).unwrap().len, 400);
    // sizes of SSND: 0 and 8 (empty), 4 (below its header), past the file, the largest; offsets
    // of its own, past the file, the largest
    for (len, offset) in [
        (0u32, 0u32),
        (8, 0),
        (4, 0),
        (1 << 30, 0),
        (u32::MAX, 0),
        (408, 4),
        (408, 400),
        (408, 1 << 30),
        (408, u32::MAX),
        (u32::MAX, u32::MAX),
    ] {
        check(&format!("{len} {offset}"), &form(b"AIFF", &[comm(2, 16), ssnd(len, offset, &samples)]));
    }
    // bits 0, 12, 24, 33, 64, channels 0, 1000
    for (ch, bits) in [(2u16, 0u16), (2, 12), (2, 24), (2, 33), (2, 64), (0, 16), (1000, 16)] {
        check(&format!("{ch} {bits}"), &form(b"AIFF", &[comm(ch, bits), ssnd(408, 0, &samples)]));
    }
    // a chunk of the largest size before; odd chunks
    let mut j = b"junk".to_vec();
    j.extend(u32::MAX.to_be_bytes());
    check("junk", &form(b"AIFF", &[j, comm(2, 16), ssnd(408, 0, &samples)]));
    let mut j = b"junk".to_vec();
    j.extend(3u32.to_be_bytes());
    j.extend(b"abc\0");
    let f = check("odd", &form(b"AIFF", &[j, comm(2, 16), ssnd(408, 0, &samples)])).unwrap();
    assert_eq!(f.len, 400);
    // AIFC: the compression type missing, unknown, sowt
    let mut c = comm(2, 16);
    c[4..8].copy_from_slice(&22u32.to_be_bytes());
    for (kind, taken) in [(&b"NONE"[..], true), (b"sowt", true), (b"fl32", false), (b"ima4", false)] {
        let mut cc = c.clone();
        cc.extend(kind);
        let d = form(b"AIFC", &[cc, ssnd(408, 0, &samples)]);
        assert_eq!(check("aifc", &d).is_some(), taken, "{kind:?}");
    }
    // AIFC with no room for the type
    check("aifc short", &form(b"AIFC", &[comm(2, 16), ssnd(408, 0, &samples)]));
    for n in 0..good.len() {
        check("cut", &good[..n]);
    }
}

fn bmp_header(dib: u32, w: i32, h: i32, bpp: u16, comp: u32, off: u32) -> Vec<u8> {
    let mut v = b"BM".to_vec();
    put32(&mut v, 0);
    put32(&mut v, 0);
    put32(&mut v, off);
    put32(&mut v, dib);
    if dib == 12 {
        put16(&mut v, w as u16);
        put16(&mut v, h as u16);
        put16(&mut v, 1);
        put16(&mut v, bpp);
    } else {
        put32(&mut v, w as u32);
        put32(&mut v, h as u32);
        put16(&mut v, 1);
        put16(&mut v, bpp);
        put32(&mut v, comp);
        v.resize(14 + dib as usize, 0);
    }
    v
}

#[test]
fn bmp_headers() {
    let px = |n: usize| (0..n).map(|i| (i * 5 + i / 9) as u8).collect::<Vec<u8>>();
    // 5 x 3 at 24 bits: rows of 16 bytes
    for (name, dib, h, extra) in [
        ("info", 40u32, 3i32, 0usize),
        ("top-down", 40, -3, 0),
        ("os/2", 12, 3, 0),
        ("v4", 108, 3, 0),
        ("v5", 124, 3, 0),
        ("v3 52", 52, 3, 0),
        ("v3 56", 56, 3, 0),
        ("64", 64, 3, 0),
        ("bitfields masks", 40, 3, 12),
    ] {
        let hdr_len = 14 + dib as usize + extra;
        let mut d = bmp_header(dib, 5, h, 24, 0, hdr_len as u32);
        d.resize(hdr_len, 0);
        d.extend(px(48));
        let f = check(name, &d).unwrap_or_else(|| panic!("{name}"));
        assert_eq!((f.name, f.len), ("bmp", 48), "{name}");
    }
    // sizes that overflow or are huge, an offset past the file, a plane count, a depth, a
    // compression, a dib size unknown
    for (name, d) in [
        ("huge", bmp_header(40, i32::MAX, i32::MAX, 32, 0, 54)),
        ("huge w", bmp_header(40, i32::MAX, 3, 32, 0, 54)),
        ("huge h", bmp_header(40, 5, i32::MIN, 24, 0, 54)),
        ("neg w", bmp_header(40, -5, 3, 24, 0, 54)),
        ("zero w", bmp_header(40, 0, 3, 24, 0, 54)),
        ("zero h", bmp_header(40, 5, 0, 24, 0, 54)),
        ("past", bmp_header(40, 5, 3, 24, 0, u32::MAX)),
        ("bpp 1", bmp_header(40, 5, 3, 1, 0, 54)),
        ("bpp 16", bmp_header(40, 5, 3, 16, 0, 54)),
        ("rle", bmp_header(40, 5, 3, 8, 1, 54)),
        ("jpeg", bmp_header(40, 5, 3, 24, 4, 54)),
        ("bitfields 24", bmp_header(40, 5, 3, 24, 3, 54)),
        ("dib", bmp_header(41, 5, 3, 24, 0, 55)),
        ("dib 0", bmp_header(0, 5, 3, 24, 0, 14)),
    ] {
        let mut d = d;
        d.extend(px(100));
        assert!(check(name, &d).is_none(), "{name}");
    }
    // 8 bits of rows of 8 bytes; 32 bits with masks
    let mut d = bmp_header(40, 5, -3, 8, 0, 54);
    d.extend(px(24));
    assert_eq!(check("8", &d).unwrap().len, 24);
    let mut d = bmp_header(108, 5, 3, 32, 3, 122);
    d.extend(px(60));
    assert_eq!(check("32", &d).unwrap().len, 60);
    // one byte short
    d.pop();
    assert!(check("short", &d).is_none());
}

#[allow(clippy::too_many_arguments)]
fn dicom(
    explicit: bool,
    with_meta: bool,
    rows: u16,
    cols: u16,
    bits: u16,
    frames: &str,
    planar: u16,
    pixels: &[u8],
) -> Vec<u8> {
    fn el(v: &mut Vec<u8>, explicit: bool, group: u16, elem: u16, vr: &[u8; 2], body: &[u8]) {
        v.extend(group.to_le_bytes());
        v.extend(elem.to_le_bytes());
        if explicit {
            v.extend(vr);
            if matches!(vr, b"OB" | b"OW" | b"UN" | b"SQ") {
                v.extend([0, 0]);
                v.extend((body.len() as u32).to_le_bytes());
            } else {
                v.extend((body.len() as u16).to_le_bytes());
            }
        } else {
            v.extend((body.len() as u32).to_le_bytes());
        }
        v.extend(body);
    }
    let mut v = vec![];
    if with_meta {
        v.resize(128, 0);
        v.extend(b"DICM");
        let syntax: &[u8] = if explicit { b"1.2.840.10008.1.2.1\0" } else { b"1.2.840.10008.1.2\0" };
        let mut m = vec![];
        el(&mut m, true, 2, 0x10, b"UI", syntax);
        el(&mut v, true, 2, 0, b"UL", &(m.len() as u32).to_le_bytes());
        v.extend(m);
    }
    el(&mut v, explicit, 8, 0x60, b"CS", b"OT");
    el(&mut v, explicit, 0x28, 2, b"US", &1u16.to_le_bytes());
    el(&mut v, explicit, 0x28, 6, b"US", &planar.to_le_bytes());
    if !frames.is_empty() {
        el(&mut v, explicit, 0x28, 8, b"IS", frames.as_bytes());
    }
    el(&mut v, explicit, 0x28, 0x10, b"US", &rows.to_le_bytes());
    el(&mut v, explicit, 0x28, 0x11, b"US", &cols.to_le_bytes());
    el(&mut v, explicit, 0x28, 0x100, b"US", &bits.to_le_bytes());
    el(&mut v, explicit, 0x7fe0, 0x10, b"OW", pixels);
    v
}

#[test]
fn dicom_files() {
    let px = |n: usize| (0..n).map(|i| (i * 5 + i / 9) as u8).collect::<Vec<u8>>();
    for explicit in [true, false] {
        for meta in [true, false] {
            if !explicit && !meta {
                // (a data set of implicit VR from its first byte: group 8, small element)
            }
            let d = dicom(explicit, meta, 4, 6, 8, "", 0, &px(24));
            let f = check("8", &d);
            if meta || !explicit {
                assert_eq!(f.map(|f| f.len), Some(24), "{explicit} {meta}");
            }
            let d = dicom(explicit, meta, 4, 6, 16, "3", 0, &px(144));
            if meta || !explicit {
                assert_eq!(check("frames", &d).map(|f| f.len), Some(144));
            }
        }
    }
    // pixel data's length not the size's, planar, frames of 0, negative, huge, not a number, bits
    // 12, 1, 0 and 32; sizes of 0; 65535 x 65535 with few pixels
    for (name, d) in [
        ("short", dicom(true, true, 4, 6, 8, "", 0, &px(23))),
        ("long", dicom(true, true, 4, 6, 8, "", 0, &px(25))),
        ("planar", dicom(true, true, 4, 6, 8, "", 1, &px(24))),
        ("frames 0", dicom(true, true, 4, 6, 8, "0", 0, &px(24))),
        ("frames -1", dicom(true, true, 4, 6, 8, "-1", 0, &px(24))),
        ("frames huge", dicom(true, true, 4, 6, 8, "99999999999999999999", 0, &px(24))),
        ("frames big", dicom(true, true, 4, 6, 8, "4294967296", 0, &px(24))),
        ("frames text", dicom(true, true, 4, 6, 8, "x", 0, &px(24))),
        ("bits 12", dicom(true, true, 4, 6, 12, "", 0, &px(24))),
        ("bits 1", dicom(true, true, 4, 6, 1, "", 0, &px(24))),
        ("bits 0", dicom(true, true, 4, 6, 0, "", 0, &[])),
        ("bits 32", dicom(true, true, 4, 6, 32, "", 0, &px(96))),
        ("rows 0", dicom(true, true, 0, 6, 8, "", 0, &[])),
        ("cols 0", dicom(true, true, 4, 0, 8, "", 0, &[])),
        ("big", dicom(true, true, 65535, 65535, 16, "65535", 0, &px(24))),
    ] {
        assert!(check(name, &d).is_none(), "{name}");
    }
    // a sequence of undefined length before the pixel data
    let mut d = dicom(true, true, 4, 6, 8, "", 0, &px(24));
    let at = d.len() - 24 - 12;
    d.splice(at..at, [0x08, 0, 0x40, 0x11, b'S', b'Q', 0, 0, 0xff, 0xff, 0xff, 0xff]);
    assert!(check("undefined", &d).is_none());
    // a preamble and no DICM, the meta group length past the file, a unknown transfer syntax
    let mut d = dicom(true, true, 4, 6, 8, "", 0, &px(24));
    d[140..144].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(check("meta length", &d).is_none());
    let mut d = dicom(true, true, 4, 6, 8, "", 0, &px(24));
    let at = d.windows(19).position(|w| w == b"1.2.840.10008.1.2.1").unwrap();
    d[at + 18] = b'9';
    assert!(check("syntax", &d).is_none());
}

#[test]
fn pnm_headers() {
    let px = |n: usize| (0..n).map(|i| (i * 5 + i / 9) as u8).collect::<Vec<u8>>();
    let with = |h: &[u8], n: usize| {
        let mut v = h.to_vec();
        v.extend(px(n));
        v
    };
    for (name, h, n, want) in [
        ("p5", &b"P5\n6 4\n255\n"[..], 24usize, Some(24)),
        ("p6", b"P6 2 4 255 ", 24, Some(24)),
        ("comments", b"P5\n#c\n6 4 #x\n255\n", 24, Some(24)),
        ("p16", b"P5\n3 4\n65535\n", 24, Some(24)),
        ("pam", b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\nTUPLTYPE GRAY\nENDHDR\n", 24, Some(24)),
        ("pam 4", b"P7\nWIDTH 3\nHEIGHT 2\nDEPTH 4\nMAXVAL 255\nENDHDR\n", 24, Some(24)),
        ("pam 2 byte", b"P7\nWIDTH 3\nHEIGHT 2\nDEPTH 2\nMAXVAL 1000\nENDHDR\n", 24, Some(24)),
        ("pam bw", b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 1\nMAXVAL 1\nENDHDR\n", 24, Some(24)),
        ("pam depth 5", b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 5\nMAXVAL 255\nENDHDR\n", 120, None),
        ("pam depth 0", b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 0\nMAXVAL 255\nENDHDR\n", 24, None),
        ("pam no depth", b"P7\nWIDTH 6\nHEIGHT 4\nMAXVAL 255\nENDHDR\n", 24, None),
        ("pam no end", b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\n", 24, None),
        ("pam no newline", b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\nENDHDR", 24, None),
        ("pam width huge", b"P7\nWIDTH 99999999999999999999\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\nENDHDR\n", 24, None),
        ("pam width max", b"P7\nWIDTH 18446744073709551615\nHEIGHT 4\nDEPTH 4\nMAXVAL 255\nENDHDR\n", 24, None),
        ("pam height max", b"P7\nWIDTH 4\nHEIGHT 18446744073709551615\nDEPTH 4\nMAXVAL 65535\nENDHDR\n", 24, None),
        ("pam word", b"P7\nWIDTH\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\nENDHDR\n", 24, None),
        ("pam junk", b"P7\nWIDTH x\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\nENDHDR\n", 24, None),
        ("maxval 0", b"P5\n6 4\n0\n", 24, None),
        ("maxval 65536", b"P5\n6 4\n65536\n", 48, None),
        ("width 0", b"P5\n0 4\n255\n", 24, None),
        ("digits 10", b"P5\n6000000000 4\n255\n", 24, None),
        ("no whitespace", b"P5\n6 4\n255", 24, None),
        ("two spaces", b"P5\n6 4\n255  ", 24, Some(24)),
        ("p4", b"P4\n6 4\n", 4, None),
        ("p3", b"P3\n6 4\n255\n", 24, None),
        ("short", b"P5\n6 4\n255\n", 23, None),
        ("comment no end", b"P5\n#c", 24, None),
        ("huge dims", b"P6\n99999 99999\n255\n", 24, None),
    ] {
        let d = with(h, n);
        assert_eq!(check(name, &d).map(|f| f.len), want, "{name}");
    }
}

#[test]
fn damaged_containers() {
    let samples: Vec<u8> = (0..120).map(|i| (i * 3 + i / 7) as u8).collect();
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("tiff".into(), good_tiff()),
        ("wav".into(), riff(&[wav_chunk(b"fmt ", 16, &fmt16(2, 16)), wav_chunk(b"data", 120, &samples)])),
        ("aiff".into(), form(b"AIFF", &[comm(2, 16), ssnd(128, 0, &samples)])),
        ("dicom".into(), dicom(true, true, 4, 6, 16, "2", 0, &samples[..96])),
        ("pam".into(), {
            let mut v = b"P7\nWIDTH 6\nHEIGHT 4\nDEPTH 1\nMAXVAL 255\nENDHDR\n".to_vec();
            v.extend(&samples[..24]);
            v
        }),
    ];
    let mut b = bmp_header(108, 5, -3, 24, 0, 122);
    b.extend(&samples[..48]);
    files.push(("bmp".into(), b));
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    for sub in ["data", "ima", "msadpcm"] {
        for e in std::fs::read_dir(format!("{dir}/{sub}")).unwrap().flatten().take(2) {
            files.push((e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()));
        }
    }
    for (name, d) in files {
        // (only the headers of the large files, at every place)
        let d = &d[..d.len().min(3000)];
        aqr_testkit::damaged(d, 60, 160, Duration::from_secs(30), |v| {
            let _ = container::sized(v);
            let _ = check(&name, v);
        });
    }
}
