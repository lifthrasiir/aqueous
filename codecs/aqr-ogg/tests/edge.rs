//! Edge cases of Ogg files: Vorbis streams over the size of a piece and with packets of a chunk
//! and more, Opus packets of every TOC code, and a decoder given lengths and counts the data
//! cannot hold.

use aqr_common::rng::Rng;
use aqr_formula::{Formula, Node};
use aqr_ogg::layout::Layout;
use aqr_ogg::{formula, page};
use aqr_testkit::{Refusing, damaged};
use aqr_vorbis::bits::Writer;
use aqr_vorbis::packet::{self, End, Field, Source};
use aqr_vorbis::setup::{Ident, Setup};
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn eval(f: &Formula, n: &Node) -> Vec<u8> {
    match n {
        Node::Slice { stream, start, len } => f.streams[*stream].data[*start as usize..][..*len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| eval(f, n)).collect(),
        Node::Apply(a) => {
            let inputs: Vec<Vec<u8>> = a.inputs.iter().map(|n| eval(f, n)).collect();
            let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
            let out = match a.module.name {
                "ogg-recons" => formula::decode(&a.params, &inputs, a.len).unwrap(),
                "vorbis-recons" => aqr_vorbis::formula::decode(&a.params, &inputs, a.len).unwrap(),
                m => panic!("{m}"),
            };
            assert_eq!(out.len() as u64, a.len);
            out
        }
        n => panic!("{n:?}"),
    }
}

fn applied(n: &Node, name: &str) -> usize {
    match n {
        Node::Apply(a) => (a.module.name == name) as usize + a.inputs.iter().map(|n| applied(n, name)).sum::<usize>(),
        Node::Concat(ns) => ns.iter().map(|n| applied(n, name)).sum(),
        _ => 0,
    }
}

/// The packets of the first stream of `d`.
fn packets_of(d: &[u8]) -> Vec<Vec<u8>> {
    let (mut out, mut cur) = (Vec::new(), Vec::new());
    for p in page::pages(d) {
        let mut at = p.body.start;
        for &v in &d[p.lacing.clone()] {
            cur.extend_from_slice(&d[at..at + v as usize]);
            at += v as usize;
            if v < 255 {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    out
}

/// `packets` as pages of at most `per` lacing values, granule positions `granule(last packet
/// ended)` where one ends, serial `serial`.
fn paginate(packets: &[Vec<u8>], serial: u32, per: usize, granule: &mut dyn FnMut(usize) -> u64) -> Vec<u8> {
    let mut lacing = Vec::new();
    let mut ends = Vec::new(); // the lacing index after each packet
    for p in packets {
        lacing.resize(lacing.len() + p.len() / 255, 255u8);
        lacing.push((p.len() % 255) as u8);
        ends.push(lacing.len());
    }
    let body: Vec<u8> = packets.concat();
    let (mut out, mut at, mut byte, mut seq) = (Vec::new(), 0, 0, 0u32);
    while at < lacing.len() {
        let n = per.min(lacing.len() - at);
        let values = &lacing[at..at + n];
        let len: usize = values.iter().map(|&v| v as usize).sum();
        let last = ends.iter().rposition(|&e| e > at && e <= at + n);
        let g = last.map_or(u64::MAX, &mut *granule);
        let kind =
            (at > 0 && lacing[at - 1] == 255) as u8 | ((seq == 0) as u8 * 2) | ((at + n == lacing.len()) as u8 * 4);
        page::write(&mut out, kind, g, serial, seq, values, &body[byte..byte + len]);
        (at, byte, seq) = (at + n, byte + len, seq + 1);
    }
    out
}

/// Writes a packet of random values of `setup`, as the walk asks for them.
struct Random<'a> {
    setup: &'a Setup,
    rng: Rng,
    w: Writer,
}

impl Source for Random<'_> {
    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        let v = match f {
            Field::Type => 0,
            Field::Mode => self.rng.below(self.setup.modes.len() as u64) as u32,
            Field::Nonzero { .. } => (self.rng.below(4) > 0) as u32,
            _ => self.rng.below(1 << n) as u32,
        };
        self.w.bits(v, n);
        Some(v)
    }

    fn entry(&mut self, _: Field, book: usize) -> Option<u32> {
        let b = &self.setup.codebooks[book];
        let used: Vec<u32> = (0..b.entries() as u32).filter(|&e| b.used(e)).collect();
        let e = used[self.rng.below(used.len() as u64) as usize];
        b.write(e, &mut self.w);
        Some(e)
    }
}

fn random_packet(setup: &Setup, seed: u64) -> Vec<u8> {
    let mut r = Random { setup, rng: Rng::new(seed), w: Writer::new() };
    assert_eq!(packet::walk(setup, &mut r), Ok(End::Whole));
    while !r.w.pos().is_multiple_of(8) {
        let b = r.rng.below(2) as u32;
        r.w.bit(b);
    }
    r.w.finish()
}

/// The headers and setup of venc-mono.ogg.
fn vorbis_headers() -> (Vec<Vec<u8>>, Setup) {
    let p = packets_of(&data("venc-mono.ogg"));
    let id = &p[0];
    let ident = Ident::from_exps(id[11] as usize, id[28] & 15, id[28] >> 4, false).unwrap();
    let setup = Setup::parse(&p[2], ident).unwrap();
    (p[..3].to_vec(), setup)
}

fn vorbis_file(headers: &[Vec<u8>], audio: &[Vec<u8>]) -> Vec<u8> {
    let mut d = paginate(&headers[..1], 5, 255, &mut |_| 0);
    let rest: Vec<Vec<u8>> = headers[1..].iter().chain(audio).cloned().collect();
    let mut n = 0;
    d.extend(paginate(&rest, 5, 40, &mut |_| {
        n += 960;
        n
    }));
    // the pages of the second part follow the first's: renumber by writing them again
    let all = page::pages(&d);
    let mut out = Vec::new();
    for (i, p) in all.iter().enumerate() {
        page::write(
            &mut out,
            p.kind & !2 | ((i == 0) as u8 * 2),
            p.granule,
            p.serial,
            i as u32,
            &d[p.lacing.clone()],
            &d[p.body.clone()],
        );
    }
    out
}

fn round_trip(name: &str, d: &[u8]) -> (Formula, bool) {
    let (f, taken) = formula::encode_any(d, 2).unwrap_or_else(|| panic!("{name}"));
    assert!(eval(&f, &f.root) == d, "{name}");
    (f, taken)
}

/// A Vorbis stream of over 2 MiB of packets (several pieces, each its own record), and packets
/// of a chunk (0xFFFF bytes) and more among them.
#[test]
fn vorbis_in_pieces() {
    let (headers, setup) = vorbis_headers();
    let mut audio = Vec::new();
    let mut total = 0;
    let mut seed = 0;
    while total < (2 << 20) + (300 << 10) {
        let p = random_packet(&setup, seed);
        seed += 1;
        total += p.len();
        audio.push(p);
    }
    let d = vorbis_file(&headers, &audio);
    let (f, taken) = round_trip("pieces", &d);
    assert!(taken);
    assert!(applied(&f.root, "vorbis-recons") >= 2, "{}", applied(&f.root, "vorbis-recons"));
}

#[test]
fn vorbis_with_long_packets() {
    let (headers, setup) = vorbis_headers();
    let mut rng = Rng::new(3);
    let valid: Vec<Vec<u8>> = (0..30).map(|s| random_packet(&setup, s)).collect();
    for len in [0xfffeusize, 0xffff, 0x10000, 0x1ffff, 0x20000, 70000] {
        // a packet of a valid start and a tail of `len` bytes, among valid ones
        let mut long = valid[0].clone();
        long.extend(rng.bytes(len.saturating_sub(long.len())));
        long.truncate(len.max(valid[0].len()));
        for at in [0, 10, 29] {
            let mut audio = valid.clone();
            audio.insert(at, long.clone());
            let d = vorbis_file(&headers, &audio);
            round_trip(&format!("long packet {len} at {at}"), &d);
        }
        // the whole stream of long packets, and a long packet of zeros and one of random bytes
        let audio = vec![long.clone(), vec![0; len], rng.bytes(len), long.clone()];
        round_trip(&format!("long packets {len}"), &vorbis_file(&headers, &audio));
    }
    // a long setup header (padded: the stream's audio is then left as it is) and a long
    // comment header
    let mut h = headers.clone();
    h[2].resize(0x10000 + 5, 0);
    round_trip("long setup", &vorbis_file(&h, &valid));
    let mut h = headers.clone();
    h[1].resize(0x20000, b'x');
    let (_, taken) = round_trip("long comment", &vorbis_file(&h, &valid));
    assert!(taken);
    // empty audio packets
    let audio = vec![vec![], valid[0].clone(), vec![], vec![0]];
    round_trip("empty packets", &vorbis_file(&headers, &audio));
}

/// Opus packets of every TOC byte (configs 0-31, codes 0-3, with frame counts of code 3 from 0 to
/// 63 and none): rebuilt, and their granule positions as RFC 7845 counts them, predicted.
#[test]
fn opus_toc_codes() {
    let mut head = b"OpusHead\x01\x02".to_vec();
    head.extend_from_slice(&[0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&[0; 8]);
    let mut rng = Rng::new(11);
    let mut audio: Vec<Vec<u8>> = Vec::new();
    let mut tocs = Vec::new();
    for toc in 0..=255u8 {
        tocs.push(toc);
    }
    for toc in tocs {
        let mut p = vec![toc];
        match toc & 3 {
            3 => {
                // frame count byte, any 0-63 and its flags (VBR, padding)
                p.push(rng.below(256) as u8);
                p.extend(rng.bytes(20));
            }
            _ => {
                let n = 1 + rng.below(40) as usize;
                p.extend(rng.bytes(n))
            }
        }
        audio.push(p);
    }
    // a packet of only its TOC byte of code 3, and of no bytes
    audio.push(vec![3]);
    audio.push(vec![0x03 | 8 << 3]);
    audio.push(vec![]);
    // The duration the RFC gives (an invalid count of code 3 counts as it does here: by the
    // bits as they are).
    let duration = |p: &[u8]| -> u64 {
        let Some(&toc) = p.first() else { return 0 };
        let c = toc as u64 >> 3;
        let size = match c {
            0..12 => [480, 960, 1920, 2880][c as usize % 4],
            12..16 => [480, 960][c as usize % 2],
            _ => [120, 240, 480, 960][c as usize % 4],
        };
        size * match toc & 3 {
            0 => 1,
            1 | 2 => 2,
            _ => p.get(1).map_or(0, |&c| c as u64 & 63),
        }
    };
    let mut pos = 0u64;
    let mut all = vec![head, tags];
    all.extend(audio.iter().cloned());
    let d = {
        let mut seen = 0usize;
        let mut sum = vec![0u64; all.len()];
        for (i, p) in all.iter().enumerate().skip(2) {
            pos += duration(p);
            sum[i] = pos;
        }
        paginate(&all, 9, 6, &mut |k| {
            seen += 1;
            sum[k]
        })
    };
    let (f, taken) = round_trip("opus", &d);
    assert!(!taken); // no Vorbis in it: encode takes none
    let Node::Apply(a) = &f.root else { panic!("{:?}", f.root) };
    let l = Layout::read(&eval(&f, &a.inputs[0]), d.len() as u64).unwrap();
    // the header pages' granule positions (0) and the audio pages' as the sum: all predicted
    assert!(
        l.pages.iter().all(|p| p.granule == 0),
        "{:?}",
        l.pages.iter().filter(|p| p.granule != 0).collect::<Vec<_>>()
    );
    // a stream cut after its headers
    round_trip("opus headers only", &paginate(&all[..2], 9, 6, &mut |_| 0));
    round_trip("opus one packet", &paginate(&all[..3], 9, 6, &mut |_| 480));
}

/// Lengths and counts that the data cannot hold are turned down, not allocated.
#[test]
fn hostile_lengths() {
    let d = data("venc-mono.ogg");
    let f = formula::encode_any(&d, 1).unwrap().0;
    let Node::Apply(a) = &f.root else { panic!() };
    let (layout, packets) = (eval(&f, &a.inputs[0]), eval(&f, &a.inputs[1]));
    for len in [1u64 << 36, 1 << 48, u64::MAX / 2, u64::MAX] {
        aqr_testkit::case(&format!("output of {len} bytes"));
        assert!(formula::decode(&[], &[&layout, &packets], len).is_err(), "{len}");
    }
    // a layout of billions of streams or pages
    for len in [d.len() as u64, 1 << 40, u64::MAX] {
        for n in [1u64 << 20, 1 << 32, 1 << 40, u64::MAX / 4] {
            let mut l = Vec::new();
            aqr_common::varint::put(&mut l, n);
            l.extend_from_slice(&[0; 12]);
            aqr_testkit::case(&format!("{n} streams of {len} bytes"));
            assert!(Layout::read(&l, len).is_err());
            // a stream, then n pages
            let mut l = Vec::new();
            aqr_common::varint::put(&mut l, 1);
            l.extend_from_slice(&[1, 0, 0, 0, 0, 0]);
            aqr_common::varint::put(&mut l, n);
            l.extend_from_slice(&[0; 12]);
            aqr_testkit::case(&format!("{n} pages of {len} bytes"));
            assert!(Layout::read(&l, len).is_err());
        }
    }
    assert!(aqr_testkit::most() < 1 << 28, "{}", aqr_testkit::most());
}

/// The layout and the packets of a file, damaged in every way: the decoder fails or gives
/// something, within bounds.
#[test]
fn damaged_inputs() {
    let d = data("venc-mono.ogg");
    let f = formula::encode_any(&d, 1).unwrap().0;
    let Node::Apply(a) = &f.root else { panic!() };
    let (layout, packets) = (eval(&f, &a.inputs[0]), eval(&f, &a.inputs[1]));
    let n = d.len() as u64;
    damaged(&layout, 300, 60, Duration::from_secs(20), |v| {
        let _ = formula::decode(&[], &[v, &packets], n);
        assert!(aqr_testkit::most() < 1 << 28, "{}", aqr_testkit::most());
    });
    damaged(&packets, 200, 60, Duration::from_secs(20), |v| {
        let _ = formula::decode(&[], &[&layout, v], n);
        assert!(aqr_testkit::most() < 1 << 28, "{}", aqr_testkit::most());
    });
    // the file damaged and encoded: a file the encoder takes is evaluated back
    damaged(&d[..4000], 250, 60, Duration::from_secs(20), |v| {
        if let Some((f, _)) = formula::encode_any(v, 1) {
            assert!(eval(&f, &f.root) == v);
        }
    });
}
