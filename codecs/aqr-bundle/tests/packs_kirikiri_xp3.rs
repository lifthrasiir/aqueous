use aqr_bundle::packs::kirikiri_xp3::{files, starts};

const MAGIC: &[u8; 11] = b"XP3\r\n \n\x1a\x8b\x67\x01";

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut v = vec![0x78, 0x01, 1];
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
    v.extend_from_slice(data);
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    v.extend_from_slice(&(b << 16 | a).to_be_bytes());
    v
}

fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend_from_slice(&(body.len() as u64).to_le_bytes());
    v.extend_from_slice(body);
    v
}

/// A file: name, flags, segments (flags, data as stored, original size).
type Seg<'a> = (u32, &'a [u8], u64);

fn entry(name: &str, flags: u32, segs: &[Seg], at: &mut u64, data: &mut Vec<u8>) -> Vec<u8> {
    let mut info = flags.to_le_bytes().to_vec();
    let orig: u64 = segs.iter().map(|s| s.2).sum();
    let packed: u64 = segs.iter().map(|s| s.1.len() as u64).sum();
    info.extend_from_slice(&orig.to_le_bytes());
    info.extend_from_slice(&packed.to_le_bytes());
    let units: Vec<u16> = name.encode_utf16().collect();
    info.extend_from_slice(&(units.len() as u16).to_le_bytes());
    for u in units {
        info.extend_from_slice(&u.to_le_bytes());
    }
    let mut segm = Vec::new();
    for (f, d, o) in segs {
        segm.extend_from_slice(&f.to_le_bytes());
        segm.extend_from_slice(&at.to_le_bytes());
        segm.extend_from_slice(&o.to_le_bytes());
        segm.extend_from_slice(&(d.len() as u64).to_le_bytes());
        *at += d.len() as u64;
        data.extend_from_slice(d);
    }
    let mut body = chunk(b"info", &info);
    body.extend_from_slice(&chunk(b"segm", &segm));
    body.extend_from_slice(&chunk(b"adlr", &[0; 4]));
    chunk(b"File", &body)
}

fn pack(zlib_index: bool, cushion: bool) -> Vec<u8> {
    let head = if cushion { 11 + 8 + 5 + 8 } else { 11 + 8 };
    let mut data = Vec::new();
    let mut at = head as u64;
    let mut idx = Vec::new();
    idx.extend(entry("a.txt", 0, &[(0, b"HELLO", 5)], &mut at, &mut data));
    idx.extend(entry(r"dir\b.png", 0, &[(0, b"\x89PNG", 4), (0, b"rest", 4)], &mut at, &mut data));
    idx.extend(entry("c.bin", 0, &[(1, &zlib_stored(b"zz"), 2)], &mut at, &mut data));
    idx.extend(entry("d.bin", 0, &[(1, b"1", 1), (1, b"2", 1)], &mut at, &mut data));
    idx.extend(entry("e.bin", 0x8000_0000, &[(0, b"SECRET", 6)], &mut at, &mut data));
    let mut index = Vec::new();
    if zlib_index {
        let z = zlib_stored(&idx);
        index.push(1);
        index.extend_from_slice(&(z.len() as u64).to_le_bytes());
        index.extend_from_slice(&(idx.len() as u64).to_le_bytes());
        index.extend_from_slice(&z);
    } else {
        index.push(0);
        index.extend_from_slice(&(idx.len() as u64).to_le_bytes());
        index.extend_from_slice(&idx);
    }
    let mut v = MAGIC.to_vec();
    if cushion {
        v.extend_from_slice(&19u64.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        v.push(0x80);
        v.extend_from_slice(&((head + data.len()) as u64).to_le_bytes());
    } else {
        v.extend_from_slice(&((head + data.len()) as u64).to_le_bytes());
    }
    assert_eq!(v.len(), head);
    v.extend_from_slice(&data);
    v.extend_from_slice(&index);
    v
}

#[test]
fn variants() {
    for (z, cushion) in [(false, false), (true, false), (false, true), (true, true)] {
        let d = pack(z, cushion);
        assert!(starts(&d));
        let f = files(&d).unwrap_or_else(|| panic!("{z} {cushion}"));
        let names: Vec<_> = f.iter().map(|f| String::from_utf8_lossy(&f.name).into_owned()).collect();
        assert_eq!(names, ["a.txt", "dir/b.png", "c.bin"]);
        assert_eq!(&d[f[0].extents[0].clone()], b"HELLO");
        assert_eq!(f[1].extents.len(), 2);
        assert_eq!(&d[f[1].extents[1].clone()], b"rest");
        assert_eq!(&d[f[2].extents[0].clone()][..2], &[0x78, 0x01]);
    }
}

#[test]
fn cut_and_not_a_pack() {
    let d = pack(true, false);
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            for file in f {
                assert!(file.extents.iter().all(|e| e.end <= n));
            }
        }
    }
    for i in 0..d.len() {
        for v in [0u8, 1, 0x7f, 0xff] {
            let mut m = d.clone();
            m[i] = v;
            let _ = files(&m);
        }
    }
    assert!(!starts(b"junk"));
    assert!(files(b"junk").is_none());
}
