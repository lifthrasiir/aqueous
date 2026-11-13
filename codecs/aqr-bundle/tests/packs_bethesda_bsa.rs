use aqr_bundle::packs::bethesda_bsa::{files, starts};

fn p32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// An archive of one folder `meshes\a` holding (name, data, compressed toggle) files; the data
/// as given is what is stored (for compressed ones, the stream after the original size).
fn pack(version: u32, flags: u32, embedded: bool, items: &[(&str, &[u8], bool, u32)]) -> Vec<u8> {
    let folder = b"meshes\\a\0";
    let names_len: usize = items.iter().map(|i| i.0.len() + 1).sum();
    let fsize = if version == 105 { 24 } else { 16 };
    let block = 36 + fsize;
    let mut file_data = Vec::new();
    // Layout: header, folder record, block (bzstring, records), names, data.
    let records_at = block + 1 + folder.len();
    let names_at = records_at + items.len() * 16;
    let mut at = names_at + names_len;
    let mut records = Vec::new();
    for (name, data, toggle, original) in items {
        let mut blob = Vec::new();
        if embedded {
            blob.push(name.len() as u8);
            blob.extend_from_slice(name.as_bytes());
        }
        let compressed = (flags & 4 != 0) != *toggle;
        if compressed {
            p32(&mut blob, *original);
        }
        blob.extend_from_slice(data);
        let size = blob.len() as u32 | if *toggle { 0x4000_0000 } else { 0 };
        p32(&mut records, 0); // hash (low)
        p32(&mut records, 0);
        p32(&mut records, size);
        p32(&mut records, at as u32);
        at += blob.len();
        file_data.extend_from_slice(&blob);
    }
    let mut v = b"BSA\0".to_vec();
    for x in [version, 36, flags, 1, items.len() as u32, folder.len() as u32, names_len as u32, 0] {
        p32(&mut v, x);
    }
    v.truncate(36);
    v.extend_from_slice(&[0; 8]); // hash
    p32(&mut v, items.len() as u32);
    if version == 105 {
        p32(&mut v, 0);
        v.extend_from_slice(&(block as u64 + names_len as u64).to_le_bytes());
    } else {
        p32(&mut v, (block + names_len) as u32);
    }
    v.push(folder.len() as u8);
    v.extend_from_slice(folder);
    v.extend_from_slice(&records);
    for i in items {
        v.extend_from_slice(i.0.as_bytes());
        v.push(0);
    }
    v.extend_from_slice(&file_data);
    v
}

fn bytes<'a>(d: &'a [u8], f: &aqr_disk::File) -> &'a [u8] {
    &d[f.extents[0].clone()]
}

#[test]
fn versions() {
    let items: [(&str, &[u8], bool, u32); 2] = [("a.nif", b"NIFDATA", false, 0), ("b.dds", b"STREAM", true, 99)];
    for (version, flags, embedded) in
        [(103, 3, false), (104, 3, false), (104, 3 | 0x100, true), (105, 3, false), (105, 7, false)]
    {
        let d = pack(version, flags, embedded, &items);
        assert!(starts(&d));
        let f = files(&d).unwrap();
        assert_eq!(f.len(), 2, "{version} {flags:x}");
        assert_eq!(f[0].name, b"meshes/a/a.nif");
        assert_eq!(f[1].name, b"meshes/a/b.dds");
        let default = flags & 4 != 0;
        // The first is compressed by the default, the second by the toggle.
        for (file, comp, data, len) in [(&f[0], default, &b"NIFDATA"[..], 0u64), (&f[1], !default, &b"STREAM"[..], 99)]
        {
            assert_eq!(bytes(&d, file), data);
            // 105: LZ4, Opaque with the original size; 103/104: zlib, plain.
            let want = (comp && version == 105).then_some(len);
            assert_eq!(file.compressed.as_ref().map(|c| c.len()), want, "{version} {flags:x}");
        }
    }
}

#[test]
fn cut_and_not_a_pack() {
    let d = pack(104, 3, false, &[("a.nif", b"NIFDATA", false, 0), ("b", b"xyz", false, 0)]);
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
    assert!(!starts(b"not a bsa at all, really not"));
    assert!(files(b"not a bsa at all, really not").is_none());
    assert!(files(b"").is_none());
}

#[test]
fn morrowind() {
    // Two files: records, name offsets, names, hashes, data.
    let names = b"meshes\\x.nif\0b.txt\0";
    let mut v = Vec::new();
    p32(&mut v, 0x100);
    p32(&mut v, 2 * 12 + names.len() as u32);
    p32(&mut v, 2);
    for (size, off) in [(3u32, 0u32), (2, 3)] {
        p32(&mut v, size);
        p32(&mut v, off);
    }
    p32(&mut v, 0);
    p32(&mut v, 13);
    v.extend_from_slice(names);
    v.extend_from_slice(&[0; 16]);
    v.extend_from_slice(b"abcde");
    assert!(starts(&v));
    let f = files(&v).unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].name, b"meshes/x.nif");
    assert_eq!(bytes(&v, &f[0]), b"abc");
    assert_eq!(bytes(&v, &f[1]), b"de");
    for n in 0..v.len() {
        if let Some(f) = files(&v[..n]) {
            assert!(f.iter().all(|f| f.extents[0].end <= n));
        }
    }
}
