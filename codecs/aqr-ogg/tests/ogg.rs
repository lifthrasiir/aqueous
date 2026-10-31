//! Ogg files rebuilt from their formulas: of libvorbisenc and libogg, of ffmpeg's muxer (Vorbis
//! and Opus), of FLAC's, chained and grouped streams, and data after the pages.

use std::collections::HashMap;

use aqr_common::rng::Rng;
use aqr_formula::{Formula, Node};
use aqr_ogg::layout::Layout;
use aqr_ogg::{formula, page};

const FILES: [&str; 6] = ["venc-mono.ogg", "ffmpeg-stereo.ogg", "opus.opus", "flac.oga", "chained.ogg", "grouped.ogg"];

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// What `n` evaluates to, running aqr-ogg's and aqr-vorbis's modules.
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

/// The modules applied in `n`, by name.
fn applied(n: &Node, out: &mut HashMap<&'static str, usize>) {
    if let Node::Apply(a) = n {
        *out.entry(a.module.name).or_default() += 1;
        a.inputs.iter().for_each(|n| applied(n, out));
    } else if let Node::Concat(ns) = n {
        ns.iter().for_each(|n| applied(n, out));
    }
}

#[test]
fn round_trip() {
    for name in FILES {
        let d = data(name);
        let f = formula::encode_any(&d, 1).unwrap_or_else(|| panic!("{name}")).0;
        assert_eq!(eval(&f, &f.root), d, "{name}");
        assert_eq!(formula::encode(&d, 1).is_some(), name.ends_with(".ogg"), "{name}");
        let mut m = HashMap::new();
        applied(&f.root, &mut m);
        let streams = match name {
            "chained.ogg" | "grouped.ogg" => 2,
            _ => 1,
        };
        let vorbis = if name.ends_with(".ogg") { streams } else { 0 };
        assert_eq!(m.get("vorbis-recons").copied().unwrap_or(0), vorbis, "{name}");
        // what is stored of the pages besides the packets: the layout record, a few bytes a page
        let Node::Apply(a) = &f.root else { panic!() };
        let layout = eval(&f, &a.inputs[0]);
        let pages = page::pages(&d).len();
        let l = Layout::read(&layout, d.len() as u64).unwrap();
        assert_eq!((l.streams.len(), l.pages.len()), (streams, pages), "{name}");
        // the header types and sequence numbers all as predicted
        assert!(l.pages.iter().all(|p| p.kind == 0 && p.seq == 0), "{name}: {:?}", l.pages);
        let granules = l.pages.iter().filter(|p| p.granule != 0).count();
        eprintln!(
            "{name}: {} bytes, {pages} pages, layout {} bytes, {granules} granules not predicted",
            d.len(),
            layout.len()
        );
    }
}

/// Data after the last page, and pages cut short or of a wrong CRC, end the file's pages and
/// are kept as they are.
#[test]
fn tails() {
    let d = data("venc-mono.ogg");
    let ends: Vec<usize> = page::pages(&d).iter().map(|p| p.end()).collect();
    let mut bad = d.clone();
    bad[ends[1] + 40] ^= 1;
    for t in [[&d[..], b"junk"].concat(), d[..d.len() - 1].to_vec(), bad] {
        let f = formula::encode_any(&t, 1).unwrap().0;
        assert_eq!(eval(&f, &f.root), t);
        assert!(matches!(&f.root, Node::Concat(_)));
    }
    assert!(formula::encode_any(&d[..20], 1).is_none());
    assert_eq!(formula::extent(&[&d[..], b"junk"].concat()), Some(d.len()));
}

/// Pages of `packets` (the last left open where `open`) of `serial` cut after the lacing
/// values `cuts` gives in turn (of pages of none too), granule positions at random where a
/// packet ends; flags at random where `odd`.
fn mux(rng: &mut Rng, packets: &[Vec<u8>], open: bool, serial: u32, odd: bool) -> Vec<Vec<u8>> {
    let mut lacing = Vec::new();
    for (i, p) in packets.iter().enumerate() {
        lacing.resize(lacing.len() + p.len() / 255, 255u8);
        if !(open && i + 1 == packets.len()) {
            lacing.push((p.len() % 255) as u8);
        }
    }
    let body: Vec<u8> = packets.concat();
    let (mut pages, mut at, mut byte, mut within) = (Vec::new(), 0, 0, false);
    while at < lacing.len() || pages.is_empty() {
        let most = if rng.below(4) == 0 { 256 } else { 12 };
        let n = (rng.below(most) as usize).min(lacing.len() - at);
        let values = &lacing[at..at + n];
        let len: usize = values.iter().map(|&v| v as usize).sum();
        let granule = if values.iter().any(|&v| v < 255) { rng.below(1 << 40) } else { u64::MAX };
        let mut kind = within as u8 | (pages.is_empty() as u8) << 1 | ((at + n == lacing.len()) as u8) << 2;
        if odd && rng.below(3) == 0 {
            kind ^= rng.below(256) as u8;
        }
        let seq = if odd { rng.u64() as u32 } else { pages.len() as u32 };
        let mut out = Vec::new();
        page::write(&mut out, kind, granule, serial, seq, values, &body[byte..byte + len]);
        pages.push(out);
        if let Some(&v) = values.last() {
            within = v == 255;
        }
        (at, byte) = (at + n, byte + len);
    }
    pages
}

/// Pages cut anywhere, of packets of every length (of none, of a chunk and more), streams left
/// open and grouped, and header fields other than predicted.
#[test]
fn muxed() {
    let mut rng = Rng::new(7);
    let mut cut = 0;
    for round in 0..60 {
        let streams = 1 + rng.below(3) as usize;
        let mut pages: Vec<Vec<Vec<u8>>> = (0..streams)
            .map(|s| {
                let packets: Vec<Vec<u8>> = (0..1 + rng.below(30))
                    .map(|_| {
                        let n = match rng.below(8) {
                            0 => 0,
                            1 => 255 * rng.below(4) as usize,
                            2 => 65535 * (1 + rng.below(2) as usize) + rng.below(3) as usize - 1,
                            _ => rng.below(2000) as usize,
                        };
                        rng.bytes(n)
                    })
                    .collect();
                let open = packets.last().unwrap().len().is_multiple_of(255)
                    && !packets.last().unwrap().is_empty()
                    && rng.below(2) == 0;
                mux(&mut rng, &packets, open, 100 + s as u32, round % 2 == 1)
            })
            .collect();
        // grouped: the pages of the streams interleaved, each's in order
        let mut d = Vec::new();
        while pages.iter().any(|p| !p.is_empty()) {
            let s = rng.below(streams as u64) as usize;
            if !pages[s].is_empty() {
                d.extend(pages[s].remove(0));
            }
        }
        let f = formula::encode_any(&d, 1).unwrap().0;
        assert_eq!(eval(&f, &f.root), d, "{round}");
        // cut after a page ending in a packet, and from it on
        let all = page::pages(&d);
        if let Some(p) = all.iter().find(|p| d[p.lacing.clone()].last() == Some(&255)) {
            for t in [&d[..p.end()], &d[p.end()..]] {
                let f = formula::encode_any(t, 1).unwrap().0;
                assert_eq!(eval(&f, &f.root), t, "{round}");
            }
            cut += 1;
        }
    }
    assert!(cut > 10, "{cut}");
}

/// The decoder turns down layouts and packets that do not make the file.
#[test]
fn corrupt() {
    let d = data("venc-mono.ogg");
    let f = formula::encode_any(&d, 1).unwrap().0;
    let Node::Apply(a) = &f.root else { panic!() };
    let (layout, packets) = (eval(&f, &a.inputs[0]), eval(&f, &a.inputs[1]));
    let n = d.len() as u64;
    assert_eq!(formula::decode(&[], &[&layout, &packets], n).unwrap(), d);
    assert!(formula::decode(&[], &[&layout, &packets], n - 1).is_err());
    assert!(formula::decode(&[], &[&layout, &packets], n + 1).is_err());
    assert!(formula::decode(&[0], &[&layout, &packets], n).is_err());
    assert!(formula::decode(&[], &[&layout, &packets[..packets.len() - 1]], n).is_err());
    assert!(formula::decode(&[], &[&layout[..layout.len() - 1], &packets], n).is_err());
    for i in 0..layout.len() {
        let mut l = layout.clone();
        l[i] ^= 0x40;
        if let Ok(out) = formula::decode(&[], &[&l, &packets], n) {
            assert_ne!(out, d);
        }
    }
}
