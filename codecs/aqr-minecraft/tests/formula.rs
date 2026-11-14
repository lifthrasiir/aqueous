//! Region files made up of chunks of every kind, encoded and made again.

use aqr_api::stream::{Reader, Slice};
use aqr_common::rng::Rng;
use aqr_flate::zlib::{Params, Strategy};
use aqr_formula::Node;
use aqr_minecraft::layout::{self, Streamed};
use aqr_minecraft::region::{self, EXTERNAL, HEADER, LZ4, NONE, SECTOR, ZLIB};
use aqr_minecraft::{formula, module};

/// How a chunk of [`region_of`] is written.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// zlib, of these parameters.
    Zlib(Params),
    /// zlib's header of level 6, the stream of level 1 (not made again).
    Other,
    Stored,
    Lz4,
    External,
}

const L6: Params = Params { level: 6, strategy: Strategy::Default, wbits: 15, mem: 8 };
const L9: Params = Params { level: 9, ..L6 };

/// Data that compresses as NBT does: names and numbers, of `n` bytes.
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut d = Vec::with_capacity(n + 64);
    while d.len() < n {
        match rng.below(4) {
            0 => d.extend_from_slice(b"\x0a\x00\x08sections"),
            1 => d.extend_from_slice(b"\x08\x00\x04Name\x00\x0fminecraft:stone"),
            2 => d.extend((0..8).map(|_| rng.below(4) as u8)),
            _ => d.extend((0..rng.below(40)).map(|_| rng.below(256) as u8)),
        }
    }
    d.truncate(n);
    d
}

/// A region file of `chunks` (index, kind, bytes of data), laid out in a shuffled order of
/// sectors, with stale bytes after some streams and in some free sectors, and `tail` after.
fn region_of(seed: u64, chunks: &[(u16, Kind, usize)], tail: &[u8]) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut order: Vec<usize> = (0..chunks.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let mut f = vec![0; HEADER];
    for (k, &i) in order.iter().enumerate() {
        let (index, kind, n) = chunks[i];
        let d = data(&mut rng, n);
        let (t, body) = match kind {
            Kind::Zlib(p) => (ZLIB, layout::zlib_onto(&d, p, Vec::new()).unwrap()),
            Kind::Other => {
                let mut z = layout::zlib_onto(&d, Params { level: 1, ..L6 }, Vec::new()).unwrap();
                z[..2].copy_from_slice(&[0x78, 0x9c]);
                (ZLIB, z)
            }
            Kind::Stored => (NONE, d),
            Kind::Lz4 => (LZ4, data(&mut rng, n / 3 + 1)),
            Kind::External => (ZLIB | EXTERNAL, Vec::new()),
        };
        // (every third chunk after free sectors, of stale bytes or none)
        if k % 3 == 2 {
            let stale = k % 2 == 0;
            f.extend((0..SECTOR).map(|j| if stale && j % 7 != 0 { rng.below(256) as u8 } else { 0 }));
        }
        let at = f.len();
        f.extend(((body.len() + 1) as u32).to_be_bytes());
        f.push(t);
        f.extend_from_slice(&body);
        let end = f.len().next_multiple_of(SECTOR);
        let stale = k % 4 == 1;
        while f.len() < end {
            f.push(if stale { rng.below(256) as u8 } else { 0 });
        }
        let (sector, count) = ((at / SECTOR) as u32, ((end - at) / SECTOR) as u32);
        assert!(count < 256, "a chunk of {count} sectors");
        f[4 * index as usize..][..4].copy_from_slice(&(sector << 8 | count).to_be_bytes());
        f[SECTOR + 4 * index as usize..][..4].copy_from_slice(&(1_700_000_000 + 37 * k as u32).to_be_bytes());
    }
    f.extend_from_slice(tail);
    f
}

/// The streams of the formula's application, in order.
fn inputs(f: &aqr_formula::Formula) -> (Vec<u8>, Vec<Vec<u8>>, u64) {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    let ins = a
        .inputs
        .iter()
        .map(|n| match *n {
            Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
            _ => panic!("an input not a slice"),
        })
        .collect();
    (a.params.clone(), ins, a.len)
}

/// The file made again of the inputs by the module, natively on threads and as the part module
/// takes them (in pieces, a chunk at a time).
fn both(params: &[u8], ins: &[Vec<u8>], len: u64) -> [Result<Vec<u8>, aqr_minecraft::Error>; 2] {
    let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
    let whole = module::decode(params, &refs, len, 4);
    let streamed = (|| {
        let p = layout::params(params)?;
        let header = layout::read_header(&mut Slice(&ins[0]))?;
        if ins[0].len() != HEADER {
            return Err(aqr_minecraft::corrupt());
        }
        let (mut l, mut d, mut k) = (Slice(&ins[1]), Slice(&ins[2]), Slice(&ins[3]));
        let mut layout = Reader::new(&mut l, 7);
        let mut kept = Reader::new(&mut k, 300);
        let mut chunks = Streamed { data: Reader::new(&mut d, 1000), left: ins[2].len() as u64, params: p };
        let mut out = Vec::new();
        layout::build_stream(&header, &mut layout, &mut chunks, &mut kept, len, &mut out)?;
        match out.len() as u64 == len {
            true => Ok(out),
            false => Err(aqr_minecraft::corrupt()),
        }
    })();
    [whole, streamed]
}

fn round_trip(what: &str, file: &[u8]) -> Vec<Vec<u8>> {
    assert!(region::starts(file), "{what}: not told a region file");
    let f = formula::encode(file, 3).unwrap_or_else(|| panic!("{what}: not taken"));
    let (params, ins, len) = inputs(&f);
    for out in both(&params, &ins, len) {
        assert!(out.unwrap() == file, "{what}: not made again");
    }
    ins
}

fn mixed() -> Vec<(u16, Kind, usize)> {
    let kinds = [Kind::Zlib(L6), Kind::Zlib(L6), Kind::Stored, Kind::Other, Kind::Lz4, Kind::External, Kind::Zlib(L6)];
    let mut v: Vec<(u16, Kind, usize)> =
        (0..60).map(|i| (i * 17 % 1024, kinds[i as usize % kinds.len()], 300 + 2311 * i as usize % 90_000)).collect();
    // (a chunk of many sectors, and one of no data)
    v.push((1023, Kind::Zlib(L6), 600_000));
    v.push((1000, Kind::Zlib(L6), 0));
    v
}

#[test]
fn chunks_of_every_kind_made_again() {
    let file = region_of(1, &mixed(), &[]);
    let ins = round_trip("mixed", &file);
    // (the data of every zlib chunk of level 6 and every stored one, the others kept)
    let made: usize = mixed().iter().filter(|c| matches!(c.1, Kind::Zlib(_) | Kind::Stored)).map(|c| c.2).sum();
    assert_eq!(ins[2].len(), made);
    assert_eq!(ins[0], file[..HEADER]);
    assert!(ins[3].len() < file.len() / 3, "kept {} of {}", ins[3].len(), file.len());
    // with bytes after the last sector, zero or not, of a whole sector or not
    for tail in [&[0u8; 100][..], &[0; SECTOR][..], &[1, 2, 3][..]] {
        round_trip("tail", &region_of(2, &mixed(), tail));
    }
}

#[test]
fn parameters_found_of_the_first_streams() {
    let chunks: Vec<(u16, Kind, usize)> = (0..20).map(|i| (i, Kind::Zlib(L9), 5000 + 999 * i as usize)).collect();
    let file = region_of(3, &chunks, &[]);
    let f = formula::encode(&file, 2).unwrap();
    assert_eq!(layout::params(&inputs(&f).0).unwrap(), L9);
    round_trip("level 9", &file);
    // the first streams of other parameters: those of the first's are made again, the rest kept
    let mut chunks = chunks;
    chunks[0].1 = Kind::Other;
    round_trip("first other", &region_of(4, &chunks, &[]));
}

#[test]
fn others_not_taken() {
    // no chunk made of its data
    let lz4: Vec<(u16, Kind, usize)> = (0..5).map(|i| (i, Kind::Lz4, 2000)).collect();
    let file = region_of(5, &lz4, &[]);
    assert!(region::starts(&file));
    assert!(formula::encode(&file, 1).is_none());
    // no chunk at all, a header cut short, not a region file
    assert!(!region::starts(&[0; HEADER]));
    assert!(!region::starts(&[0; 100]));
    let file = region_of(6, &mixed(), &[]);
    assert!(!region::starts(&file[..HEADER - 1]));
    assert!(formula::encode(&file[..HEADER + 10], 1).is_none());
    let text = b"lorem ipsum dolor sit amet ".repeat(2000);
    assert!(!region::starts(&text));
    // two chunks of a sector, a chunk past the end
    let mut bad = file.clone();
    bad[..4].copy_from_slice(&0x0000_0201u32.to_be_bytes());
    bad[4..8].copy_from_slice(&0x0000_0201u32.to_be_bytes());
    assert!(!region::starts(&bad) && formula::encode(&bad, 1).is_none());
    let mut bad = file.clone();
    bad[..4].copy_from_slice(&(((file.len() / SECTOR) as u32) << 8 | 1).to_be_bytes());
    assert!(!region::starts(&bad) && formula::encode(&bad, 1).is_none());
}

/// Every byte of every input flipped in turn (of a small file): an error or another file, never
/// a panic, natively and streamed alike.
#[test]
fn flipped_inputs() {
    let chunks = [(0, Kind::Zlib(L6), 3000), (5, Kind::Stored, 400), (9, Kind::Lz4, 900), (31, Kind::Zlib(L6), 7000)];
    let file = region_of(7, &chunks, &[9, 9]);
    let f = formula::encode(&file, 1).unwrap();
    let (params, ins, len) = inputs(&f);
    for k in 1..4 {
        let step = (ins[k].len() / 300).max(1);
        for i in (0..ins[k].len()).step_by(step) {
            let mut ins = ins.clone();
            ins[k][i] ^= 0x41;
            let [a, b] = both(&params, &ins, len);
            assert_eq!(a.is_ok(), b.is_ok(), "input {k} byte {i}");
            if let (Ok(a), Ok(b)) = (a, b) {
                assert!(a == b, "input {k} byte {i}");
            }
        }
    }
    // the header's locations: overlapping, past the end
    for i in (0..SECTOR).step_by(4) {
        let mut ins = ins.clone();
        ins[0][i + 2] ^= 0x10;
        let [a, b] = both(&params, &ins, len);
        assert_eq!(a.is_ok(), b.is_ok(), "header byte {i}");
    }
    // another length, other params
    let [a, b] = both(&params, &ins, len + SECTOR as u64);
    assert!(a.is_err() && b.is_err());
    for p in [&[][..], &[0, 0, 15, 8], &[6, 9, 15, 8], &[6, 0, 15, 8, 0]] {
        let [a, b] = both(p, &ins, len);
        assert!(a.is_err() && b.is_err(), "{p:?}");
    }
}

/// The plan of the module follows the largest chunk rebuilt, not the file.
#[test]
fn plan_of_the_largest_chunk() {
    let plan = |chunks: &[(u16, Kind, usize)]| {
        let file = region_of(8, chunks, &[]);
        let f = formula::encode(&file, 2).unwrap();
        let (params, ins, len) = inputs(&f);
        module::plan(0, &params, &ins[0], &ins[1], len).unwrap()
    };
    let few: Vec<_> = (0..4).map(|i| (i, Kind::Zlib(L6), 50_000)).collect();
    let many: Vec<_> = (0..400).map(|i| (i, Kind::Zlib(L6), 50_000)).collect();
    assert_eq!(plan(&few), plan(&many));
    let mut big = few.clone();
    big.push((900, Kind::Zlib(L6), 1_500_000));
    assert!(plan(&big) > plan(&few) + 1_500_000);
}
