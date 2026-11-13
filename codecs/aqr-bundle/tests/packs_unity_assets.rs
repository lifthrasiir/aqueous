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
