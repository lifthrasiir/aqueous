use aqr_bundle::packs::unity_assets::{files, starts};

/// Writes of the metadata in an endianness.
struct W {
    b: Vec<u8>,
    big: bool,
}

impl W {
    fn u16(&mut self, v: u16) {
        self.b.extend(if self.big { v.to_be_bytes() } else { v.to_le_bytes() });
    }
    fn u32(&mut self, v: u32) {
        self.b.extend(if self.big { v.to_be_bytes() } else { v.to_le_bytes() });
    }
    fn u64(&mut self, v: u64) {
        self.b.extend(if self.big { v.to_be_bytes() } else { v.to_le_bytes() });
    }
    fn str(&mut self, s: &str) {
        self.b.extend(s.as_bytes());
        self.b.push(0);
    }
    /// A tree of the older layout: a node with `children` leaves.
    fn old_tree(&mut self, children: u32) {
        self.str("Base");
        self.str("Base");
        for v in [-1i32 as u32, 0, 0, 1, 0x8000, children] {
            self.u32(v);
        }
        for k in 0..children {
            self.str("int");
            self.str(&format!("m_F{k}"));
            for v in [4, k + 1, 0, 1, 0, 0] {
                self.u32(v);
            }
        }
    }
}

/// A serialized file of version `v` holding `objects` (id, class, contents), with type trees or
/// not, and the names and extents expected.
fn build(
    v: u32,
    big: bool,
    trees: bool,
    objects: &[(i64, i32, &[u8])],
) -> (Vec<u8>, Vec<(String, std::ops::Range<usize>)>) {
    let head = if v >= 22 { 48 } else { 20 };
    let mut w = W { b: vec![0; head], big };
    w.str("2019.4.1f1");
    w.u32(19);
    if v >= 13 {
        w.b.push(trees as u8);
    }
    let mut classes: Vec<i32> = Vec::new();
    for o in objects {
        if !classes.contains(&o.1) {
            classes.push(o.1);
        }
    }
    w.u32(classes.len() as u32);
    for &c in &classes {
        w.u32(c as u32);
        if v >= 16 {
            w.b.push(0);
        }
        if v >= 17 {
            w.u16(if c == 114 { 0 } else { 0xffff });
        }
        if v >= 13 {
            if (v < 16 && c < 0) || (v >= 16 && c == 114) {
                w.b.extend([0x5a; 16]);
            }
            w.b.extend([0xa5; 16]);
        }
        if v < 13 || trees {
            if v >= 12 || v == 10 {
                let nodes = 3;
                let strings = b"Base\0m_Name\0int\0";
                w.u32(nodes);
                w.u32(strings.len() as u32);
                w.b.extend(vec![0x11; nodes as usize * if v >= 19 { 32 } else { 24 }]);
                w.b.extend(strings);
            } else {
                w.old_tree(2);
            }
            if v >= 21 {
                w.u32(2);
                w.u32(7);
                w.u32(8);
            }
        }
    }
    if (7..14).contains(&v) {
        w.u32(0);
    }
    w.u32(objects.len() as u32);
    // (the objects' data: each at a multiple of 8)
    let mut start = 0u64;
    let mut starts = Vec::new();
    for (id, class, data) in objects {
        if v >= 14 {
            while !w.b.len().is_multiple_of(4) {
                w.b.push(0);
            }
            w.u64(*id as u64);
        } else {
            w.u32(*id as u32);
        }
        if v >= 22 {
            w.u64(start);
        } else {
            w.u32(start as u32);
        }
        w.u32(data.len() as u32);
        let ty = classes.iter().position(|c| c == class).unwrap() as u32;
        w.u32(if v >= 16 { ty } else { *class as u32 });
        if v < 16 {
            w.u16(*class as u16);
        }
        if v < 11 {
            w.u16(0);
        }
        if (11..17).contains(&v) {
            w.u16(0xffff);
        }
        if v == 15 || v == 16 {
            w.b.push(0);
        }
        starts.push(start);
        start = (start + data.len() as u64).next_multiple_of(8);
    }
    // (scripts and externals: none)
    w.u32(0);
    w.u32(0);
    let meta = w.b.len() - head;
    let data_at = w.b.len().next_multiple_of(16).max(4096);
    let mut b = w.b;
    b.resize(data_at, 0);
    let mut expect = Vec::new();
    for ((id, _, data), s) in objects.iter().zip(&starts) {
        let at = data_at + *s as usize;
        b.resize(at, 0);
        b.extend(*data);
        expect.push((id, at..at + data.len()));
    }
    let size = b.len();
    b[8..12].copy_from_slice(&v.to_be_bytes());
    b[16] = big as u8;
    if v >= 22 {
        b[20..24].copy_from_slice(&(meta as u32).to_be_bytes());
        b[24..32].copy_from_slice(&(size as u64).to_be_bytes());
        b[32..40].copy_from_slice(&(data_at as u64).to_be_bytes());
    } else {
        b[0..4].copy_from_slice(&(meta as u32).to_be_bytes());
        b[4..8].copy_from_slice(&(size as u32).to_be_bytes());
        b[12..16].copy_from_slice(&(data_at as u32).to_be_bytes());
    }
    let names = ["tex2d", "mesh", "mono", "c1"];
    let classes_of = [28, 43, 114, 1];
    let expect = expect
        .into_iter()
        .zip(objects)
        .map(|((id, r), o)| (format!("{id}.{}", names[classes_of.iter().position(|&c| c == o.1).unwrap()]), r))
        .collect();
    (b, expect)
}

const OBJECTS: &[(i64, i32, &[u8])] =
    &[(1, 1, b"GameObject"), (2, 28, b"texture data here"), (-7, 114, b"mono"), (40000000000, 43, b"a mesh's bytes")];

fn check(v: u32, big: bool, trees: bool) {
    // (32-bit ids before version 14)
    let objects: Vec<_> =
        OBJECTS.iter().map(|&(id, c, d)| (if v < 14 { id as i32 as i64 } else { id }, c, d)).collect();
    let (b, expect) = build(v, big, trees, &objects);
    assert!(starts(&b), "v{v}");
    let got = files(&b).unwrap_or_else(|| panic!("v{v} big {big} trees {trees}"));
    let got: Vec<_> = got.iter().map(|f| (String::from_utf8(f.name.clone()).unwrap(), f.extents[0].clone())).collect();
    assert_eq!(got, expect, "v{v} big {big} trees {trees}");
    for (_, r) in &got {
        assert!(r.end <= b.len());
    }
    // cut short anywhere: None (the file's size is past the end)
    for n in (0..b.len()).step_by(7) {
        assert!(files(&b[..n]).is_none(), "v{v} cut at {n}");
    }
    // the metadata garbled: never a panic, extents within
    for k in 20..b.len().min(400) {
        let mut c = b.clone();
        c[k] ^= 0x80;
        if let Some(fs) = files(&c) {
            assert!(fs.iter().all(|f| f.extents.iter().all(|r| r.end <= c.len())));
        }
    }
}

#[test]
fn versions() {
    for v in 9..=22 {
        for big in [false, true] {
            check(v, big, true);
            if v >= 13 {
                check(v, big, false);
            }
        }
    }
}

#[test]
fn followed_by_more() {
    // a bundle's node: the file followed by other bytes
    let (mut b, expect) = build(21, false, true, OBJECTS);
    b.extend([0xee; 100]);
    let got = files(&b).unwrap();
    assert_eq!(got.len(), expect.len());
}

#[test]
fn not_serialized() {
    assert!(!starts(b"UnityFS\0\0\0\0\x07 5.x.x\0"));
    assert!(files(&[0u8; 100]).is_none());
    let (mut b, _) = build(23, false, false, OBJECTS);
    assert!(files(&b).is_none());
    b[8..12].copy_from_slice(&8u32.to_be_bytes());
    assert!(files(&b).is_none());
}

/// A `Texture2D` object of Unity 2019.4 (name, fallback format, downscale, then width, height,
/// image size, format, mipmaps, other fields, the image's data, the streamed data): its blocks
/// inline (`data`), or streamed from `path` at `offset` (`data` empty, `size` the blocks').
fn texture2d(
    w: u32,
    h: u32,
    format: u32,
    mips: u32,
    size: u32,
    data: &[u8],
    stream: Option<(u32, u32, &str)>,
) -> Vec<u8> {
    let mut o = Vec::new();
    let str4 = |o: &mut Vec<u8>, s: &[u8]| {
        o.extend((s.len() as u32).to_le_bytes());
        o.extend(s);
        o.resize(o.len().next_multiple_of(4), 0);
    };
    str4(&mut o, b"brick_albedo");
    o.extend(4u32.to_le_bytes());
    o.push(0);
    o.resize(o.len().next_multiple_of(4), 0);
    for v in [w, h, size, format, mips] {
        o.extend(v.to_le_bytes());
    }
    // (readable, streaming, image count, dimension, the sampler's fields, lightmap, colour space)
    o.extend([0, 0, 0, 0]);
    for v in [0u32, 1, 2, 1, 1, 0, 0, 0, 0, 0, 1] {
        o.extend(v.to_le_bytes());
    }
    o.extend((data.len() as u32).to_le_bytes());
    o.extend(data);
    let (offset, s, path) = stream.unwrap_or((0, 0, ""));
    o.extend(offset.to_le_bytes());
    o.extend(s.to_le_bytes());
    str4(&mut o, path.as_bytes());
    o
}

/// BC1 blocks of `w` x `h` and its mipmaps: bytes of a gradient, so that aqr-tex has something
/// to code.
fn bc1(w: u32, h: u32, mips: u32) -> Vec<u8> {
    let l = aqr_tex::layout::Layout { format: aqr_tex::layout::Format::Bc1, width: w, height: h, mips, layers: 1 };
    (0..l.len()).map(|i| (i * 7 / 3) as u8 ^ (i as u8 >> 3)).collect()
}

fn eval(f: &aqr_formula::Formula) -> Vec<u8> {
    f.eval(&|a: &aqr_formula::Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, aqr_tex::formula::MODULE);
        aqr_tex::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
    })
    .unwrap()
}

/// Textures read: their blocks inline or streamed, of the formats taken, of the size their
/// layout makes; the serialized file's inline blocks and a resource file's streamed ones
/// rebuilt by aqr-tex.
#[test]
fn textures() {
    use aqr_bundle::packs::unity_assets::{
        Data, Pixels, Raw, encode_resource, encode_with, objects, streamed, texture,
    };
    let blocks = bc1(64, 32, 7);
    let n = blocks.len() as u32;
    let inline = texture2d(64, 32, 10, 7, n, &blocks, None);
    let t = texture(&inline, (2019, 4), false).unwrap();
    let Pixels::Blocks(l) = t.pixels else { panic!("blocks") };
    assert_eq!((l.width, l.height, l.mips, l.layers), (64, 32, 7, 1));
    let Data::Inline(r) = t.data else { panic!("inline") };
    assert!(inline[r] == blocks[..]);
    let at = 1000;
    let streamed_obj = texture2d(64, 32, 10, 7, n, &[], Some((at, n, "sharedassets0.assets.resS")));
    let t = texture(&streamed_obj, (2019, 4), false).unwrap();
    assert_eq!(t.data, Data::Stream { path: b"sharedassets0.assets.resS".to_vec(), offset: at as u64, size: n as u64 });
    // RGBA32 (format 4) of its mipmaps (64 x 32 down to 1 x 1), uncompressed
    let rgba: u32 = (0..7).map(|m| (64u32 >> m).max(1) * (32u32 >> m).max(1) * 4).sum();
    let t = texture(&texture2d(64, 32, 4, 7, rgba, &vec![9; rgba as usize], None), (2019, 4), false).unwrap();
    assert_eq!(t.pixels, Pixels::Raw(Raw { channels: 4, width: 64, height: 32, mips: 7, layers: 1 }));
    // not taken: RGBA32 of a size other than its mipmaps', a size other than the layout's, BC7 (25)
    assert!(texture(&texture2d(64, 32, 4, 7, 64 * 32 * 4, &[], None), (2019, 4), false).is_none());
    assert!(texture(&texture2d(64, 32, 10, 7, n + 8, &blocks, None), (2019, 4), false).is_none());
    assert!(texture(&texture2d(64, 32, 25, 7, n * 2, &[], None), (2019, 4), false).is_none());
    // a serialized file of both, and the resource file
    let (d, _) = build(21, false, true, &[(1, 1, b"GameObject"), (2, 28, &inline), (3, 28, &streamed_obj)]);
    assert_eq!(objects(&d).unwrap().engine, (2019, 4));
    let f = encode_with(&d, |_| aqr_formula::Kind::Unknown, 1).unwrap();
    assert_eq!(f.applies().len(), 1);
    assert!(eval(&f) == d);
    let mut ress = vec![0x33u8; at as usize];
    ress.extend(&blocks);
    ress.extend(b"after");
    let t = streamed(&d, b"archive:/x/SHAREDASSETS0.assets.resS");
    assert_eq!(t.len(), 1);
    let f = encode_resource(&ress, &t, 1).unwrap();
    assert!(eval(&f) == ress);
    assert!(encode_resource(&ress[..1100], &t, 1).is_none());
}

/// A UnityFS bundle (format 7, its table stored, one stored block) of `nodes` (path, bytes).
fn bundle(nodes: &[(&str, &[u8])]) -> Vec<u8> {
    let data: Vec<u8> = nodes.iter().flat_map(|n| n.1.iter().copied()).collect();
    let mut t = vec![0u8; 16];
    t.extend(1u32.to_be_bytes());
    t.extend((data.len() as u32).to_be_bytes());
    t.extend((data.len() as u32).to_be_bytes());
    t.extend(0u16.to_be_bytes());
    t.extend((nodes.len() as u32).to_be_bytes());
    let mut at = 0u64;
    for (name, b) in nodes {
        t.extend(at.to_be_bytes());
        t.extend((b.len() as u64).to_be_bytes());
        t.extend(4u32.to_be_bytes());
        t.extend(name.as_bytes());
        t.push(0);
        at += b.len() as u64;
    }
    let mut d = b"UnityFS\0".to_vec();
    d.extend(7u32.to_be_bytes());
    d.extend(b"5.x.x\0" as &[u8]);
    d.extend(b"2019.4.1f1\0" as &[u8]);
    let size_at = d.len();
    d.extend([0; 8]);
    d.extend((t.len() as u32).to_be_bytes());
    d.extend((t.len() as u32).to_be_bytes());
    d.extend(0x40u32.to_be_bytes());
    d.resize(d.len().next_multiple_of(16), 0);
    d.extend(t);
    d.extend(data);
    let n = d.len() as u64;
    d[size_at..size_at + 8].copy_from_slice(&n.to_be_bytes());
    d
}

/// A bundle's resource node, of the textures its serialized node streams from it: their blocks
/// rebuilt by aqr-tex, the bundle given back; of no texture streamed, no such formula.
#[test]
fn bundle_resources() {
    let blocks = bc1(64, 32, 7);
    let n = blocks.len() as u32;
    let path = "archive:/CAB-0123/CAB-0123.resS";
    let obj = texture2d(64, 32, 10, 7, n, &[], Some((16, n, path)));
    let (cab, _) = build(21, false, true, &[(1, 1, b"GameObject"), (2, 28, &obj)]);
    let mut ress = vec![0x44u8; 16];
    ress.extend(&blocks);
    let d = bundle(&[("CAB-0123", &cab), ("CAB-0123.resS", &ress)]);
    let f = aqr_bundle::packs::unity::encode_with(&d, |_| aqr_formula::Kind::Unknown, 1).unwrap();
    assert_eq!(f.applies().len(), 1);
    assert!(eval(&f) == d);
    let plain = bundle(&[("CAB-0123", &cab), ("other.resS", &ress)]);
    assert!(aqr_bundle::packs::unity::encode_with(&plain, |_| aqr_formula::Kind::Unknown, 1).is_none());
}

/// A texture streamed from an offset so far that offset + size wraps: none taken.
#[test]
fn streamed_offset_overflow() {
    use aqr_bundle::packs::unity_assets::streamed;
    let blocks = bc1(64, 32, 7);
    let n = blocks.len() as u32;
    let path = "a.resS";
    let wide = |low: u32, high: u32| {
        let mut obj = texture2d(64, 32, 10, 7, n, &[], Some((low, n, path)));
        // (2020.1 on: the offset is 64 bits)
        let at = obj.len() - (path.len().next_multiple_of(4) + 4 + 8);
        obj.splice(at + 4..at + 4, high.to_le_bytes());
        // (and the mipmaps stripped, after the size)
        obj.splice(36..36, 0u32.to_le_bytes());
        let (mut d, _) = build(21, false, true, &[(1, 28, &obj)]);
        let v = d.windows(10).position(|w| w == b"2019.4.1f1").unwrap();
        d[v..v + 10].copy_from_slice(b"2020.1.1f1");
        d
    };
    assert_eq!(streamed(&wide(100, 0), b"a.resS").len(), 1);
    assert!(streamed(&wide(u32::MAX - 5, u32::MAX), b"a.resS").is_empty());
}
