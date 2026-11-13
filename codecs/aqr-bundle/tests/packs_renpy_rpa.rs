use aqr_bundle::packs::renpy_rpa::{files, starts};

pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut v = vec![0x78, 0x01];
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        v.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(c) = chunks.next() {
        v.push(chunks.peek().is_none() as u8);
        v.extend_from_slice(&(c.len() as u16).to_le_bytes());
        v.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        v.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    v.extend_from_slice(&(b << 16 | a).to_be_bytes());
    v
}

/// Python 2 style (str, tuples of ints) or 3 (unicode, `_codecs.encode` prefixes).
fn index(entries: &[(&str, u32, u32, Option<&str>)], key: u32, py3: bool) -> Vec<u8> {
    let mut p = vec![0x80, 2, b'}', b'q', 0, b'('];
    let mut memo = 1u8;
    let mut put = |p: &mut Vec<u8>| {
        p.extend_from_slice(&[b'q', memo]);
        memo += 1;
    };
    for (name, off, len, prefix) in entries {
        if py3 {
            p.push(b'X');
            p.extend_from_slice(&(name.len() as u32).to_le_bytes());
        } else {
            p.push(b'U');
            p.push(name.len() as u8);
        }
        p.extend_from_slice(name.as_bytes());
        put(&mut p);
        p.extend_from_slice(b"]");
        put(&mut p);
        p.push(b'(');
        p.push(b'J');
        p.extend_from_slice(&(off ^ key).to_le_bytes());
        p.push(b'J');
        p.extend_from_slice(&(len ^ key).to_le_bytes());
        if let Some(pre) = prefix {
            if py3 {
                p.extend_from_slice(b"c_codecs\nencode\n");
                put(&mut p);
                p.push(b'X');
                p.extend_from_slice(&(pre.len() as u32).to_le_bytes());
                p.extend_from_slice(pre.as_bytes());
                put(&mut p);
                p.extend_from_slice(&[b'X', 6, 0, 0, 0]);
                p.extend_from_slice(b"latin1");
                put(&mut p);
                p.extend_from_slice(&[0x86, b'R']);
                put(&mut p);
            } else {
                p.push(b'U');
                p.push(pre.len() as u8);
                p.extend_from_slice(pre.as_bytes());
            }
            p.push(0x87);
        } else {
            p.push(0x86);
        }
        put(&mut p);
        p.push(b'e');
    }
    p.extend_from_slice(b"u.");
    p
}

fn pack(entries: &[(&str, &[u8], Option<&str>)], key: Option<u32>, py3: bool) -> Vec<u8> {
    let header_len = if key.is_some() { 8 + 16 + 1 + 8 + 1 } else { 8 + 16 + 1 };
    let mut data = Vec::new();
    let mut es = Vec::new();
    for (n, d, p) in entries {
        es.push((*n, (header_len + data.len()) as u32, d.len() as u32, *p));
        data.extend_from_slice(d);
    }
    let offset = header_len + data.len();
    let mut v = match key {
        Some(k) => format!("RPA-3.0 {offset:016x} {k:08x}\n").into_bytes(),
        None => format!("RPA-2.0 {offset:016x}\n").into_bytes(),
    };
    assert_eq!(v.len(), header_len);
    v.extend_from_slice(&data);
    v.extend_from_slice(&zlib_stored(&index(&es, key.unwrap_or(0), py3)));
    v
}

#[test]
fn versions() {
    let items: [(&str, &[u8], Option<&str>); 3] =
        [("images/a.png", b"\x89PNG....", None), ("script.rpyc", b"RENPY RPC2", Some("pre")), ("b.ogg", b"OggS", None)];
    for (key, py3) in [(Some(0x42424242), false), (Some(0x1234567), true), (None, false), (None, true)] {
        let d = pack(&items, key, py3);
        assert!(starts(&d));
        let f = files(&d).unwrap_or_else(|| panic!("{key:?} {py3}"));
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].name, b"images/a.png");
        assert_eq!(&d[f[0].extents[0].clone()], b"\x89PNG....");
        assert_eq!(&d[f[1].extents[0].clone()], b"RENPY RPC2");
        assert_eq!(&d[f[2].extents[0].clone()], b"OggS");
    }
}

#[test]
fn cut_and_not_a_pack() {
    let d = pack(&[("a", b"xyz", None), ("bb", b"1234567", Some("p"))], Some(7), true);
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            assert!(f.iter().all(|f| f.extents[0].end <= n));
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
    assert!(files(b"RPA-3.0 0000000000000000 00000000\n").is_none());
    // Unknown opcodes are refused.
    let mut v = b"RPA-2.0 0000000000000020\n".to_vec();
    v.resize(32, 0);
    v.extend_from_slice(&zlib_stored(b"\x80\x02N."));
    assert!(files(&v).is_none());
}
