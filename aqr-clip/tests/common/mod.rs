//! Synthetic `.clip` files: the container, block data of tiles (streams zlib makes, empty blocks,
//! and streams it would not make: stored blocks), external data of another kind, a database and
//! the footer. Shared with aqr-compress's and aqr-decompress's tests.

#![allow(dead_code)]

use aqr_clip::tile::{AREA, Layout, SIDE};

/// A string as the files have them: a u32 count of UTF-16BE units, then them.
pub fn utf16(s: &str) -> Vec<u8> {
    let mut v = (s.len() as u32).to_be_bytes().to_vec();
    v.extend(s.encode_utf16().flat_map(|u| u.to_be_bytes()));
    v
}

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// `d` as a zlib stream of stored blocks, which zlib at level 1 would not make.
pub fn stored_zlib(d: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let n = d.chunks(65535).count();
    for (i, c) in d.chunks(65535).enumerate() {
        z.push((i + 1 == n) as u8);
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    z.extend(adler32(d).to_be_bytes());
    z
}

/// A tile of layout `l` from `seed`: flat fills, a line, a gradient and a patch of noise over
/// a transparent background, the fourth byte 0 or 255.
pub fn tile(l: Layout, seed: u32) -> Vec<u8> {
    let mut t = vec![0u8; l.bytes()];
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let fill = [(rnd() % 200) as u8, (rnd() % 200) as u8, (rnd() % 200) as u8];
    let (cx, cy, r) = (rnd() as usize % SIDE, rnd() as usize % SIDE, 30 + rnd() as usize % 90);
    for y in 0..SIDE {
        for i in 0..SIDE {
            let p = y * SIDE + i;
            let inside = (i as isize - cx as isize).pow(2) + (y as isize - cy as isize).pow(2) < (r * r) as isize;
            let line = (i + y) % 97 < 2;
            let grad = y > 200 && i < 100;
            let noise = y < 16 && i > 240;
            let (a, c): (u8, [u8; 3]) = if noise {
                (rnd() as u8, [rnd() as u8, rnd() as u8, rnd() as u8])
            } else if line {
                (255, [10, 10, 10])
            } else if grad {
                (255, [i as u8, (y * 2) as u8, 128])
            } else if inside {
                (255, fill)
            } else {
                (0, [0, 0, 0])
            };
            match l {
                Layout::Color => {
                    t[p] = a;
                    t[AREA + 4 * p..AREA + 4 * p + 4].copy_from_slice(&[c[0], c[1], c[2], if a > 0 { 0 } else { 255 }]);
                }
                Layout::One => t[p] = c[1] ^ a,
                Layout::Two => {
                    t[p] = a;
                    t[AREA + p] = c[0];
                }
            }
        }
    }
    t
}

/// A block of block data: none, or a tile of layout `l` and its zlib stream.
pub enum Block {
    Empty,
    Tile(Layout, Vec<u8>),
}

/// Block data of `blocks`, then their status and checksum tables.
pub fn block_data(blocks: &[Block]) -> Vec<u8> {
    let mut d = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let mut r = utf16("BlockDataBeginChunk");
        let (len, z): (usize, &[u8]) = match b {
            Block::Empty => (Layout::Color.bytes(), &[]),
            Block::Tile(l, z) => (l.bytes(), z),
        };
        for v in [i as u32, len as u32, 256, 256, !z.is_empty() as u32] {
            r.extend(v.to_be_bytes());
        }
        if !z.is_empty() {
            r.extend((z.len() as u32 + 4).to_be_bytes());
            r.extend((z.len() as u32).to_le_bytes());
            r.extend_from_slice(z);
        }
        r.extend(utf16("BlockDataEndChunk"));
        d.extend((r.len() as u32 + 4).to_be_bytes());
        d.extend(r);
    }
    for (name, v) in [("BlockStatus", 1u32), ("BlockCheckSum", 0x1234_5678)] {
        d.extend(utf16(name));
        for h in [12, blocks.len() as u32, 4] {
            d.extend(h.to_be_bytes());
        }
        for i in 0..blocks.len() {
            d.extend((v ^ i as u32).to_be_bytes());
        }
    }
    d
}

/// The body of an `Exta` chunk of ID number `n` and data `data`.
pub fn external(n: u32, data: &[u8]) -> Vec<u8> {
    let id = format!("extrnlid{n:032X}");
    let mut b = (id.len() as u64).to_be_bytes().to_vec();
    b.extend(id.as_bytes());
    b.extend((data.len() as u64).to_be_bytes());
    b.extend_from_slice(data);
    b
}

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = b"CHNK".to_vec();
    c.extend_from_slice(kind);
    c.extend((body.len() as u64).to_be_bytes());
    c.extend_from_slice(body);
    c
}

/// A `.clip` file of the bodies of `Exta` chunks and a database (where given).
pub fn clip_file(externals: &[Vec<u8>], database: Option<&[u8]>) -> Vec<u8> {
    let mut f = b"CSFCHUNK".to_vec();
    f.extend([0u8; 8]);
    f.extend(24u64.to_be_bytes());
    f.extend(chunk(b"Head", &[7u8; 40]));
    for e in externals {
        f.extend(chunk(b"Exta", e));
    }
    if let Some(db) = database {
        f.extend(chunk(b"SQLi", db));
    }
    f.extend(chunk(b"Foot", &[]));
    let n = f.len() as u64;
    f[8..16].copy_from_slice(&n.to_be_bytes());
    f
}

/// A colour tile's block, its stream as zlib makes it.
pub fn made(l: Layout, seed: u32) -> Block {
    Block::Tile(l, aqr_clip::zlib::deflate(&tile(l, seed)).unwrap())
}

/// A file of every kind of part: tiles of the three layouts, empty blocks, a stream zlib would
/// not make, `colour` more colour tiles, external data of another kind, and a database.
pub fn sample(colour: u32) -> Vec<u8> {
    let mut a = vec![made(Layout::Color, 1), Block::Empty, made(Layout::Color, 2)];
    a.push(Block::Tile(Layout::Color, stored_zlib(&tile(Layout::Color, 3))));
    a.push(made(Layout::One, 4));
    a.extend((0..colour).map(|k| made(Layout::Color, 100 + k)));
    let b = vec![made(Layout::One, 5), Block::Empty, made(Layout::Two, 6)];
    let mut other = b"\x14\0\0\0CLIP_STUDIO_3D_DATA2".to_vec();
    other.extend((0..5000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
    let db = [&b"not a database, but where one is "[..], &[3u8; 3000]].concat();
    clip_file(&[external(1, &block_data(&a)), external(2, &other), external(3, &block_data(&b))], Some(&db))
}
