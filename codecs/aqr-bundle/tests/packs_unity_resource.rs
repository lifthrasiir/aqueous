use aqr_bundle::packs::unity_resource::{files, starts};

/// An FSB5 bank of version `v` with parts of the sizes given (samples' headers, names, data).
fn bank(v: u32, parts: [u32; 3]) -> Vec<u8> {
    let mut b = b"FSB5".to_vec();
    for x in [v, 1, parts[0], parts[1], parts[2], 15] {
        b.extend(x.to_le_bytes());
    }
    b.resize(if v == 0 { 64 } else { 60 }, 0x33);
    for (k, &n) in parts.iter().enumerate() {
        b.extend(vec![0x40 + k as u8; n as usize]);
    }
    b
}

fn webm(n: usize) -> Vec<u8> {
    let mut b = vec![0x1a, 0x45, 0xdf, 0xa3, 0x84, 1, 2, 3, 4];
    // (the segment's size in 4 bytes)
    b.extend([0x18, 0x53, 0x80, 0x67, 0x10]);
    b.extend(&(n as u32).to_be_bytes()[1..]);
    b.extend(vec![0x77; n]);
    b
}

fn mp4(n: usize) -> Vec<u8> {
    let mut b = 16u32.to_be_bytes().to_vec();
    b.extend(b"ftypmp42\0\0\0\0");
    b.extend(b"\0\0\0\x0cmoov\x01\x02\x03\x04");
    b.extend((8 + n as u32).to_be_bytes());
    b.extend(b"mdat");
    b.extend(vec![0x55; n]);
    b
}

fn names(d: &[u8]) -> Option<Vec<(String, std::ops::Range<usize>)>> {
    Some(files(d)?.into_iter().map(|f| (String::from_utf8(f.name).unwrap(), f.extents[0].clone())).collect())
}

#[test]
fn banks_and_videos() {
    let members = [bank(1, [16, 8, 100]), bank(0, [32, 0, 7]), webm(50), mp4(33), bank(1, [0, 0, 1])];
    let mut d = Vec::new();
    let mut expect = Vec::new();
    for (k, m) in members.iter().enumerate() {
        // (padding to 16 bytes after some)
        if k % 2 == 1 {
            d.resize(d.len().next_multiple_of(16), 0);
        }
        let ext = ["fsb", "fsb", "webm", "mp4", "fsb"][k];
        expect.push((format!("{k:03}.{ext}"), d.len()..d.len() + m.len()));
        d.extend(m);
    }
    assert!(starts(&d));
    assert_eq!(names(&d).unwrap(), expect);
    // something else after: the rest
    d.extend(b"\0\0\0junk");
    assert_eq!(names(&d).unwrap(), expect);
    // cut short anywhere: the members that fit, or None; never a panic
    for n in 0..d.len() {
        if let Some(got) = names(&d[..n]) {
            assert!(got.iter().all(|(_, r)| r.end <= n));
            assert!(got.len() <= expect.len() && got[..] == expect[..got.len()], "{n}: {got:?}");
        } else {
            // (a bank alone is aqr-fsb's: none where nothing follows the first)
            assert!(n <= expect[0].1.end);
        }
    }
}

#[test]
fn not_banks() {
    assert!(!starts(b"FSB4\0\0\0\0"));
    assert!(names(b"FSB5\x02\0\0\0").is_none());
    assert!(names(&webm(10)).is_none());
    // a bank larger than the file
    assert!(names(&bank(1, [16, 8, 100])[..150]).is_none());
}
