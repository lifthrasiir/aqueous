//! Edge cases of the packs of game engines: hostile bytes with the start of each format (every
//! reader, not only the few of `packs_fuzz.rs`), and entries that overlap, repeat or are empty.

use aqr_archive::Evaluate;
use aqr_bundle::packs::{self, Pack};
use aqr_formula::Kind;
use aqr_testkit::{Refusing, damaged, within};
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(20);

fn none(_: &aqr_formula::Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!("no module")
}

fn kind(_: &[u8]) -> Kind {
    Kind::Unknown
}

/// What a reader makes of `d` must be right: extents within `d`, a formula rebuilding it.
fn check(pack: Pack, d: &[u8]) -> bool {
    let Some(files) = pack.files(d) else { return false };
    for f in &files {
        for e in &f.extents {
            assert!(e.start <= e.end && e.end <= d.len(), "{} extent {e:?} of {}", pack.name(), d.len());
        }
    }
    if let Some(f) = packs::encode_with(pack, d, kind) {
        assert!(f.eval(&none).unwrap() == d, "{} formula", pack.name());
    }
    true
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Mostly zeros and small bytes, as the fields of a directory are.
    fn byte(&mut self) -> u8 {
        let r = self.next();
        match r % 4 {
            0 => 0,
            1 | 2 => (r >> 8) as u8 % 6,
            _ => (r >> 8) as u8,
        }
    }
}

fn put(d: &mut [u8], at: usize, b: &[u8]) {
    d[at..at + b.len()].copy_from_slice(b);
}

/// A body of `n` bytes starting as pack `p` does (its fields of the header small, so that a
/// reader goes on), the tail where a format is known by its end.
fn hostile(p: Pack, n: usize, rng: &mut Rng) -> Vec<u8> {
    let mut d: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
    let len = n as u32;
    match p {
        Pack::GodotPck => {
            put(&mut d, 0, b"GDPC");
            put(&mut d, 4, &(1 + rng.next() as u32 % 4).to_le_bytes());
        }
        Pack::UnityBundle => {
            put(&mut d, 0, b"UnityFS\0");
            put(&mut d, 8, &(6 + rng.next() as u32 % 3).to_be_bytes());
        }
        Pack::UnrealPak => {
            let tail = [44, 204, 172][rng.next() as usize % 3];
            put(&mut d, n - tail, &[0xE1, 0x12, 0x6F, 0x5A]);
            put(&mut d, n - tail + 4, &(1 + rng.next() as u32 % 11).to_le_bytes());
            put(&mut d, n - tail + 8, &(rng.next() % 64).to_le_bytes());
            put(&mut d, n - tail + 16, &(rng.next() % 64).to_le_bytes());
        }
        Pack::SourceVpk => {
            put(&mut d, 0, &[0x34, 0x12, 0xAA, 0x55]);
            put(&mut d, 4, &(1 + rng.next() as u32 % 2).to_le_bytes());
        }
        Pack::UnrealUpk => {
            put(&mut d, 0, &[0xC1, 0x83, 0x2A, 0x9E]);
            let v = if rng.next().is_multiple_of(2) { 0x20000u32 } else { 500 + rng.next() as u32 % 400 };
            put(&mut d, 4, &v.to_le_bytes());
        }
        Pack::FmodBank => {
            put(&mut d, 0, b"RIFF");
            put(&mut d, 8, b"FEV ");
        }
        Pack::WwiseBnk => {
            put(&mut d, 0, b"BKHD");
            let v = 1 + rng.next() as u32 % 150;
            put(&mut d, 8, &if rng.next().is_multiple_of(2) { v.to_le_bytes() } else { v.to_be_bytes() });
        }
        Pack::WwisePck => {
            put(&mut d, 0, b"AKPK");
            put(&mut d, 4, &(16 + rng.next() as u32 % 32).to_le_bytes());
        }
        Pack::XactXwb => {
            put(&mut d, 0, b"WBND");
            put(&mut d, 4, &(4 + rng.next() as u32 % 40).to_le_bytes());
        }
        Pack::CriAwb => {
            put(&mut d, 0, b"AFS2");
            d[5] = [2, 4][rng.next() as usize % 2];
            d[6] = [2, 4][rng.next() as usize % 2];
        }
        Pack::CriAcb => put(&mut d, 0, b"@UTF"),
        Pack::CriCpk => put(&mut d, 0, b"CPK "),
        Pack::CriAfs => put(&mut d, 0, b"AFS\0"),
        Pack::BethesdaBsa => {
            put(&mut d, 0, b"BSA\0");
            put(&mut d, 4, &(103 + rng.next() as u32 % 3).to_le_bytes());
        }
        Pack::BethesdaBa2 => {
            put(&mut d, 0, b"BTDX");
            put(&mut d, 4, &[1u32, 2, 3, 7, 8][rng.next() as usize % 5].to_le_bytes());
            put(&mut d, 8, if rng.next().is_multiple_of(2) { b"GNRL" } else { b"DX10" });
        }
        Pack::EaBig => put(&mut d, 0, if rng.next().is_multiple_of(2) { b"BIGF" } else { b"BIG4" }),
        Pack::RenpyRpa => put(&mut d, 0, if rng.next().is_multiple_of(2) { b"RPA-3.0 " } else { b"RPA-2.0 " }),
        Pack::KirikiriXp3 => put(&mut d, 0, b"XP3\r\n \n\x1a\x8b\x67\x01"),
        Pack::MtfArc => {
            put(&mut d, 0, b"ARC\0");
            put(&mut d, 4, &[rng.next() as u8 % 8, 0]);
            put(&mut d, 6, &(rng.next() as u16 % 3).to_le_bytes());
        }
        Pack::SonyPsarc => {
            put(&mut d, 0, b"PSAR");
            put(&mut d, 8, if rng.next().is_multiple_of(2) { b"zlib" } else { b"lzma" });
        }
        Pack::UnityAssets => {
            put(&mut d, 8, &[0, 0, 0, [9, 15, 17, 19, 22][rng.next() as usize % 5]]);
            d[16] = (rng.next() % 2) as u8;
            // (version 22's fields; the older ones' lie within them)
            put(&mut d, 20, &(1 + rng.next() as u32 % 8).to_be_bytes());
            put(&mut d, 24, &(n as u64 - rng.next() % 8).to_be_bytes());
            put(&mut d, 32, &(48 + rng.next() % 16).to_be_bytes());
        }
        Pack::UnityResource => {
            put(&mut d, 0, b"FSB5");
            put(&mut d, 4, &(rng.next() as u32 % 2).to_le_bytes());
            // (a bank that fits, and with luck another, or a video, after it)
            for at in [12, 16, 20] {
                put(&mut d, at, &(rng.next() as u32 % 24).to_le_bytes());
            }
            if rng.next().is_multiple_of(2) {
                put(&mut d, 100, b"FSB5");
            }
        }
        Pack::GamemakerWin => {
            put(&mut d, 0, b"FORM");
            put(&mut d, 4, &(len - 8).to_le_bytes());
            put(&mut d, 8, if rng.next().is_multiple_of(2) { b"GEN8" } else { b"AUDO" });
        }
        Pack::ChromiumPak => {
            // (version 4, a few resources, the first right after the entries)
            let count = rng.next() as usize % 4;
            let end = 9 + 6 * (count + 1);
            put(&mut d, 0, &4u32.to_le_bytes());
            put(&mut d, 4, &(count as u32).to_le_bytes());
            d[8] = (rng.next() % 4) as u8;
            for i in 0..=count {
                let off = if i == count { n } else { end + (n - end) * i / (count + 1) };
                put(&mut d, 9 + 6 * i + 2, &(off as u32).to_le_bytes());
            }
            put(&mut d, 9 + 2, &(end as u32).to_le_bytes());
        }
        Pack::QuakePak => {
            put(&mut d, 0, b"PACK");
            let entries = 1 + rng.next() as usize % 3;
            put(&mut d, 4, &((n - 64 * entries) as u32).to_le_bytes());
            put(&mut d, 8, &(64 * entries as u32).to_le_bytes());
        }
        Pack::DoomWad => {
            put(&mut d, 0, if rng.next().is_multiple_of(2) { b"IWAD" } else { b"PWAD" });
            let entries = 1 + rng.next() as usize % 3;
            put(&mut d, 4, &(entries as u32).to_le_bytes());
            put(&mut d, 8, &((n - 16 * entries) as u32).to_le_bytes());
        }
        Pack::QuakeWad | Pack::HlWad => {
            put(&mut d, 0, if p == Pack::QuakeWad { b"WAD2" } else { b"WAD3" });
            let entries = 1 + rng.next() as usize % 3;
            put(&mut d, 4, &(entries as u32).to_le_bytes());
            put(&mut d, 8, &((n - 32 * entries) as u32).to_le_bytes());
        }
        Pack::GmodGma => {
            put(&mut d, 0, b"GMAD");
            d[4] = 1 + (rng.next() % 3) as u8;
        }
        Pack::XnaXnb => {
            put(&mut d, 0, b"XNB");
            d[4] = 3 + (rng.next() % 3) as u8;
            put(&mut d, 6, &len.to_le_bytes());
        }
    }
    d
}

/// Every reader on inputs that start as its format does and go on as a hostile directory: no
/// panic, no hang, no allocation the bytes cannot hold, and what is read is right. Not
/// vacuous: most readers start on many inputs.
#[test]
fn every_pack_on_hostile_directories() {
    let mut rng = Rng(0x2545f4914f6cdd1d);
    let mut report = Vec::new();
    for p in Pack::ALL {
        let (mut started, mut read) = (0, 0);
        for i in 0..3000 {
            let n = 260 + (rng.next() as usize % 300) + i % 7 * 40;
            let d = hostile(p, n, &mut rng);
            started += p.starts(&d) as u32;
            let name = format!("{} #{i}", p.name());
            aqr_testkit::case(&name);
            let r = within(LIMIT, || check(p, &d)).unwrap_or_else(|e| panic!("{name}: {e}\n{d:?}"));
            read += r as u32;
        }
        report.push(format!("{}: started {started}, read {read}", p.name()));
    }
    eprintln!("{}", report.join("\n"));
    let dead: Vec<_> = report.iter().filter(|l| l.contains("started 0")).collect();
    assert!(dead.is_empty(), "no input started: {dead:?}");
}

// --- entries that overlap, repeat or are empty

fn quake_pak(files: &[(&str, u32, u32)], data: &[u8]) -> Vec<u8> {
    let mut d = b"PACK".to_vec();
    d.extend(((12 + data.len()) as u32).to_le_bytes());
    d.extend((64 * files.len() as u32).to_le_bytes());
    d.extend(data);
    for (n, o, s) in files {
        let mut e = [0u8; 64];
        e[..n.len()].copy_from_slice(n.as_bytes());
        e[56..60].copy_from_slice(&o.to_le_bytes());
        e[60..64].copy_from_slice(&s.to_le_bytes());
        d.extend(e);
    }
    d
}

fn rebuilds(p: Pack, d: &[u8]) -> Option<usize> {
    let f = packs::encode_with(p, d, kind)?;
    assert!(f.eval(&none).unwrap() == d);
    let c = aqr_archive::encode(&f, None, aqr_archive::Level::LOW).unwrap();
    let runner = aqr_archive::natives(&[], &none);
    assert!(aqr_archive::decode(&c, &runner).unwrap() == d, "archived");
    Some(f.streams.len())
}

#[test]
fn overlapping_duplicate_and_empty_entries() {
    let data: Vec<u8> = (0..200u32).map(|i| (i * 7 % 251) as u8).collect();
    // (the data starts at 12)
    let cases: [&[(&str, u32, u32)]; 7] = [
        &[("a", 12, 100), ("b", 62, 100)],
        &[("a", 12, 100), ("b", 12, 100)],
        &[("a", 12, 100), ("b", 12, 50), ("c", 40, 10)],
        &[("a", 12, 0), ("b", 20, 0), ("c", 30, 50)],
        &[("a", 12, 100), ("a", 112, 100)],
        &[("a", 0, 100), ("b", 212, 64)],
        &[("a", 0, 12 + 200 + 64 * 2), ("b", 12, 10)],
    ];
    for (i, files) in cases.iter().enumerate() {
        let d = quake_pak(files, &data);
        assert!(Pack::QuakePak.starts(&d), "{i}");
        within(LIMIT, || {
            check(Pack::QuakePak, &d);
            rebuilds(Pack::QuakePak, &d)
        })
        .unwrap_or_else(|e| panic!("case {i}: {e}"));
        damaged(&d, 40, 40, LIMIT, |v| {
            check(Pack::QuakePak, v);
        });
    }
}

/// A pack whose one file is all of it: no formula (the contents would be the pack again,
/// detected again without end); a pack as the data of another is a formula.
#[test]
fn a_pack_whose_one_file_is_a_pack() {
    let inner = quake_pak(&[("x", 12, 4)], b"abcd");
    let whole = quake_pak(&[("self", 0, (12 + 4 + 64) as u32)], b"abcd");
    assert_eq!(whole.len(), 12 + 4 + 64);
    assert!(Pack::QuakePak.starts(&whole));
    assert_eq!(Pack::QuakePak.files(&whole).unwrap().len(), 1);
    assert!(packs::encode_with(Pack::QuakePak, &whole, kind).is_none());
    let outer = quake_pak(&[("inner.pak", 12, inner.len() as u32), ("y", 12, 3)], &inner);
    assert_eq!(packs::detect(&outer), Some(Pack::QuakePak));
    let files = Pack::QuakePak.files(&outer).unwrap();
    assert_eq!(&outer[files[0].extents[0].clone()], &inner[..]);
    assert!(rebuilds(Pack::QuakePak, &outer).is_some());
    assert_eq!(packs::detect(&outer[12..12 + inner.len()]), Some(Pack::QuakePak));
}
