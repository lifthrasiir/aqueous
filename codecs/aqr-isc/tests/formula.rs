//! Cabinets made of chunks that aqr-flate's zlib emulation writes (level 6, 9, as InstallShield's
//! zlib does), taken apart, evaluated natively and compared with the cabinet.

use aqr_common::rng::Rng;
use aqr_decompress::Native;
use aqr_flate::zlib::{Params, compress};
use aqr_formula::Formula;
use aqr_isc::formula::{self, CHUNK, Options};
use aqr_isc::isc;

/// Text-like data of `n` bytes.
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let words: Vec<&[u8]> =
        vec![b"install ", b"shield ", b"cabinet ", b"file ", b"group ", b"\r\n", b"0123456789", b"{}"];
    let mut d = Vec::with_capacity(n + 16);
    while d.len() < n {
        match rng.below(8) {
            0 => d.extend((0..rng.below(40)).map(|_| rng.below(256) as u8)),
            _ => d.extend_from_slice(words[rng.below(words.len() as u64) as usize]),
        }
    }
    d.truncate(n);
    d
}

/// A cabinet of files, each cut in chunks of [`CHUNK`] bytes of data (the last of what is left),
/// each chunk a DEFLATE stream of zlib at `level` after its length; the files' data too.
fn cabinet(files: &[Vec<u8>], level: u8) -> Vec<u8> {
    let mut d = isc::MAGIC.to_vec();
    d.extend([0xb0, 0x04, 0x00, 0x02]);
    d.extend([0; 4]);
    d.extend(0x200u32.to_le_bytes());
    d.extend([0; 4]);
    d.extend(0x200u32.to_le_bytes());
    d.resize(0x200, 0);
    for f in files {
        for c in f.chunks(CHUNK) {
            let z = compress(c, Params { level, ..Params::default() }).unwrap();
            d.extend((z.len() as u16).to_le_bytes());
            d.extend(z);
        }
    }
    d
}

fn taken(d: &[u8], opt: &Options) -> Option<(Formula, usize)> {
    let e = formula::encode_with(d, opt)?;
    assert!(e.formula.eval(&Native::default()).unwrap() == d);
    Some((e.formula, e.rebuilt))
}

#[test]
fn files_of_chunks_are_rebuilt() {
    let mut rng = Rng::new(1);
    for level in [6, 9] {
        let files = vec![
            data(&mut rng, 3 * CHUNK + 777),
            data(&mut rng, 100),
            data(&mut rng, CHUNK),
            data(&mut rng, 2 * CHUNK),
            data(&mut rng, 40_000),
        ];
        let d = cabinet(&files, level);
        assert!(formula::starts(&d));
        let (f, rebuilt) = taken(&d, &Options::default()).expect("taken");
        assert_eq!(rebuilt, files.iter().map(|f| f.len().div_ceil(CHUNK)).sum::<usize>());
        // a file's data is a content of its own, but for the small one (the 100 bytes share a
        // stream); a file of a full chunk ends with the next, so that its data and the next
        // files' (to the one of a chunk of less) are one: two contents
        let contents = f.streams.iter().filter(|s| s.content).count();
        assert_eq!(contents, 2, "level {level}");
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::FLATE));
    }
}

#[test]
fn what_is_kept() {
    let mut rng = Rng::new(2);
    // (chunks of a few bytes are kept: their records would cost more)
    let files = vec![data(&mut rng, 2 * CHUNK + 900), data(&mut rng, CHUNK + 900)];
    let good = cabinet(&files, 6);
    let cab = isc::parse(&good).unwrap();
    // a chunk that is no DEFLATE stream (its length and bytes damaged) is kept as it is, with the
    // files' data before and after it
    let mut bad = good.clone();
    let second = cab.chunks[1].clone();
    bad[second.start + 3..second.start + 9].fill(0xff);
    let (_, rebuilt) = taken(&bad, &Options::default()).expect("the others are rebuilt");
    assert_eq!(rebuilt, cab.chunks.len() - 1);
    // a limit short of the chunks: the rest is kept
    let (_, rebuilt) = taken(&good, &Options { limit: 2 * CHUNK as u64, ..Options::default() }).expect("two");
    assert_eq!(rebuilt, 2);
    assert!(formula::encode_with(&good, &Options { limit: 0, ..Options::default() }).is_none());
    // bytes after the last chunk (a cut one, or what follows) are kept
    let mut tail = good.clone();
    tail.extend([9, 0, 1, 2, 3]);
    taken(&tail, &Options::default()).expect("with a tail");
}

#[test]
fn what_is_no_cabinet() {
    assert!(!formula::starts(b"ISc(short"));
    assert!(formula::encode(b"MSCF\0\0\0\0", 1).is_none());
    let mut rng = Rng::new(3);
    let mut d = cabinet(&[data(&mut rng, 1000)], 6);
    d[0] = b'X';
    assert!(formula::encode(&d, 1).is_none());
}
