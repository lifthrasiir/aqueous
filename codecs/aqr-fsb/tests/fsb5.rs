//! FSB5 banks made here: their layout, and the formula of their head and samples giving them
//! back.

use aqr_formula::{Apply, Formula};
use aqr_fsb::formula;
use aqr_fsb::fsb5::{Codec, Layout, extent, sample_sums, starts};

fn eval(f: &Formula) -> Vec<u8> {
    assert!(f.applies().is_empty());
    f.eval(&|_: &Apply, _: &[&[u8]]| unreachable!()).unwrap()
}

/// A bank of version 1 and `mode`, of samples of `sizes` bytes (multiples of 32), the first with
/// a chunk of 4 bytes, and `names` bytes of names.
fn bank(mode: u32, sizes: &[usize], names: usize) -> Vec<u8> {
    let mut headers = Vec::new();
    let mut offset = 0;
    for (i, &n) in sizes.iter().enumerate() {
        let more = (i == 0) as u64;
        let h = more | 8 << 1 | (offset as u64 / 32) << 7 | 1000 << 34;
        headers.extend_from_slice(&h.to_le_bytes());
        if i == 0 {
            // a chunk of type 2 (the frequency), 4 bytes, the last
            headers.extend_from_slice(&(2u32 << 25 | 4 << 1).to_le_bytes());
            headers.extend_from_slice(&44100u32.to_le_bytes());
        }
        offset += n;
    }
    let mut d = b"FSB5".to_vec();
    for v in [1, sizes.len() as u32, headers.len() as u32, names as u32, offset as u32, mode] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.resize(0x3C, 0xab);
    d.extend_from_slice(&headers);
    d.extend((0..names).map(|i| b'a' + (i % 26) as u8));
    for (i, &n) in sizes.iter().enumerate() {
        d.extend((0..n).map(|k| (k * 7 + i * 13) as u8));
    }
    d
}

#[test]
fn layout_of_banks() {
    let d = bank(15, &[64, 96, 32], 20);
    let l = Layout::of(&d).unwrap();
    let head = 0x3C + 8 * 3 + 8 + 20;
    assert_eq!((l.codec, l.head), (Codec::Vorbis, head));
    assert_eq!(l.samples, [head..head + 64, head + 64..head + 160, head + 160..head + 192]);
    assert_eq!(l.headers.iter().map(|h| (h.channels, h.chunks.len())).collect::<Vec<_>>(), [(1, 1), (1, 0), (1, 0)]);
    assert_eq!(l.headers[0].chunk(&d, 2), Some(&44100u32.to_le_bytes()[..]));
    assert_eq!(extent(&d), Some(d.len()));
    assert_eq!(Layout::of(&bank(7, &[32], 0)).unwrap().codec, Codec::ImaAdpcm);
    assert_eq!(Layout::of(&bank(17, &[32], 0)).unwrap().codec, Codec::Other(17));
    // bytes after a bank are not its
    let mut longer = d.clone();
    longer.extend_from_slice(b"trailing");
    assert_eq!(extent(&longer), Some(d.len()));
    // the sums of samples of at least 64 bytes
    assert_eq!(sample_sums(&d, 64).iter().map(|s| s.1).collect::<Vec<_>>(), [64, 96]);
}

/// A bank is known by its start alone (the head of a file, as aqr-compress looks at it for the
/// samples files share), though its data is not there.
#[test]
fn known_by_its_start() {
    let d = bank(15, &[64, 96, 32], 20);
    let head = 0x3C + 8 * 3 + 8 + 20;
    for n in [0x3C + 8, head, head + 10, d.len() - 1] {
        assert!(starts(&d[..n]), "{n}");
        assert!(Layout::of(&d[..n]).is_none());
    }
    // but not by its magic alone, nor with sample headers that do not parse
    assert!(!starts(&d[..0x3C]));
    let mut e = d.clone();
    e[0x3C + 16] = 0xff;
    e[0x3C + 17] = 0xff;
    assert!(!starts(&e[..head]));
}

#[test]
fn not_banks() {
    let d = bank(15, &[64, 96], 4);
    assert!(starts(&d));
    let corrupt = |at: usize, v: u8| {
        let mut e = d.clone();
        e[at] = v;
        e
    };
    // magic, version, no samples, sample headers' size; data past the end (a bank's start, but
    // no layout)
    assert!(!starts(&corrupt(0, b'G')));
    assert!(!starts(&corrupt(4, 2)));
    assert!(!starts(&corrupt(8, 0)));
    assert!(!starts(&corrupt(12, 30)));
    assert!(Layout::of(&d[..d.len() - 1]).is_none() && extent(&d[..d.len() - 1]).is_none());
    // offsets out of order (the second sample's before the first's end is fine; before 0 is not)
    let mut e = d.clone();
    let p = 0x3C + 8 + 8;
    let h = u64::from_le_bytes(e[p..p + 8].try_into().unwrap()) | ((1 << 27) - 1) << 7;
    e[p..p + 8].copy_from_slice(&h.to_le_bytes());
    assert!(!starts(&e));
    for n in 0..d.len() {
        let _ = Layout::of(&d[..n]);
    }
}

#[test]
fn formula_gives_banks_back() {
    for d in [bank(15, &[64, 96, 32], 20), bank(7, &[32], 0), bank(2, &[32, 0, 64], 3)] {
        let f = formula::encode(&d, 1).unwrap();
        assert_eq!(eval(&f), d);
        let mut longer = d.clone();
        longer.extend_from_slice(b"after");
        assert_eq!(eval(&formula::encode(&longer, 1).unwrap()), longer);
    }
    assert!(formula::encode(b"FSB5", 1).is_none());
}

/// Writes a packet of random values of `setup`, as the walk asks for them (floors mostly used).
struct Random<'a> {
    setup: &'a aqr_vorbis::setup::Setup,
    rng: aqr_common::rng::Rng,
    w: aqr_vorbis::bits::Writer,
}

impl aqr_vorbis::packet::Source for Random<'_> {
    fn bits(&mut self, f: aqr_vorbis::packet::Field, n: u32) -> Option<u32> {
        use aqr_vorbis::packet::Field;
        let v = match f {
            Field::Type => 0,
            Field::Mode => self.rng.below(self.setup.modes.len() as u64) as u32,
            Field::Nonzero { .. } => (self.rng.below(4) > 0) as u32,
            _ => self.rng.below(1 << n) as u32,
        };
        self.w.bits(v, n);
        Some(v)
    }

    fn entry(&mut self, _: aqr_vorbis::packet::Field, book: usize) -> Option<u32> {
        let b = &self.setup.codebooks[book];
        let used: Vec<u32> = (0..b.entries() as u32).filter(|&e| b.used(e)).collect();
        let e = used[self.rng.below(used.len() as u64) as usize];
        b.write(e, &mut self.w);
        Some(e)
    }
}

/// A Vorbis bank of one sample of `channels` (1, 2 or 6) and `packets` random packets, of the
/// database's setup of CRC-32 `crc`: framed packets, a length of 0, padding to 32 bytes.
fn vorbis_bank(crc: u32, channels: usize, packets: u64) -> Vec<u8> {
    let db = aqr_fsb::setups::database().unwrap();
    let setup = db.assemble_row(db.row(crc).unwrap()).unwrap();
    let ident = aqr_vorbis::setup::Ident { channels, blocksizes: [256, 2048] };
    let s = aqr_vorbis::setup::Setup::parse(&setup, ident).unwrap();
    let mut data = Vec::new();
    for seed in 0..packets {
        let mut r = Random { setup: &s, rng: aqr_common::rng::Rng::new(seed), w: aqr_vorbis::bits::Writer::new() };
        aqr_vorbis::packet::walk(&s, &mut r).unwrap();
        let p = r.w.finish();
        data.extend_from_slice(&(p.len() as u16).to_le_bytes());
        data.extend_from_slice(&p);
    }
    data.extend_from_slice(&[0, 0]);
    data.resize(data.len().next_multiple_of(32), 0);
    let mut headers = Vec::new();
    let code = [1, 2, 6].iter().position(|&c| c == channels).unwrap() as u64;
    headers.extend_from_slice(&(1u64 | 9 << 1 | code << 5 | 1000 << 34).to_le_bytes());
    headers.extend_from_slice(&(11u32 << 25 | 4 << 1).to_le_bytes());
    headers.extend_from_slice(&crc.to_le_bytes());
    let mut d = b"FSB5".to_vec();
    for v in [1, 1, headers.len() as u32, 0, data.len() as u32, 15] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.resize(0x3C, 0);
    d.extend_from_slice(&headers);
    d.extend_from_slice(&data);
    d
}

#[test]
fn vorbis_samples_through_the_module() {
    // mono, stereo (coupled), and six channels of a mono setup, as FMOD codes them
    for (crc, channels) in [(0x632b1fac, 1), (0x988e56d5, 2), (0x632b1fac, 6)] {
        let d = vorbis_bank(crc, channels, 40);
        let f = formula::encode(&d, 2).unwrap();
        assert_eq!(f.applies().len(), 1);
        let out = f
            .eval(&|a: &Apply, inputs: &[&[u8]]| {
                assert_eq!(a.module, aqr_vorbis::formula::MODULE);
                aqr_vorbis::formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
            })
            .unwrap();
        assert_eq!(out, d);
    }
    // a CRC-32 the database does not have: the sample is kept as it is
    let mut d = vorbis_bank(0x632b1fac, 1, 5);
    let at = 0x3C + 12;
    d[at..at + 4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    assert!(formula::encode(&d, 1).unwrap().applies().is_empty());
}
