//! `slack` of asar archives.

use super::*;

/// An asar archive of `files` (name, bytes, unpacked), the bytes of the data in that order
/// with `stale` bytes between and after them.
fn asar(files: &[(&str, &[u8], bool)], stale: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut entries = Vec::new();
    for (name, d, unpacked) in files {
        if *unpacked {
            entries.push(format!("\"{name}\":{{\"size\":{},\"unpacked\":true}}", d.len()));
            continue;
        }
        data.extend_from_slice(stale);
        entries.push(format!("\"{name}\":{{\"size\":{},\"offset\":\"{}\"}}", d.len(), data.len()));
        data.extend_from_slice(d);
    }
    data.extend_from_slice(stale);
    let json =
        format!("{{\"files\":{{\"dir\":{{\"files\":{{{}}}}},\"l\":{{\"link\":\"dir/a\"}}}}}}", entries.join(","));
    let padded = json.len().next_multiple_of(4);
    let mut d = Vec::new();
    d.extend(4u32.to_le_bytes());
    d.extend((padded as u32 + 8).to_le_bytes());
    d.extend((padded as u32 + 4).to_le_bytes());
    d.extend((json.len() as u32).to_le_bytes());
    d.extend(json.as_bytes());
    d.resize(16 + padded, 0);
    d.extend(data);
    d
}

#[test]
fn bytes_of_no_file_zeroed() {
    let files: [(&str, &[u8], bool); 3] = [("a", b"alpha alpha", false), ("b", b"beta", true), ("c", b"gamma!", false)];
    let x = asar(&files, b"stale");
    assert!(aqr_asar::starts(&x));
    let Some((Rewrite::Made { format, .. }, Some(r))) = rewrite(&x, &parse_any("slack").unwrap(), &Options::default())
    else {
        panic!("rewritten")
    };
    assert_eq!(format, Format::Asar);
    assert_eq!(r, asar(&files, b"\0\0\0\0\0"));
    check::asar(&x, &r).unwrap();
    assert!(rewrite(&r, &parse_any("slack").unwrap(), &Options::default()).is_none());
    // a file's byte, the header, a byte of no file made other than zero
    let at_a = r.windows(11).position(|w| w == b"alpha alpha").unwrap();
    for (at, what) in [(at_a, "a file"), (20, "header"), (at_a - 1, "but to zeros")] {
        let mut bad = r.clone();
        bad[at] ^= 0x41;
        assert!(check::asar(&x, &bad).unwrap_err().contains(what), "{at}");
    }
    // (an offset not a number: kept, told why)
    let mut bad = x.clone();
    let at = bad.windows(12).position(|w| w == b"\"offset\":\"5\"").unwrap();
    bad[at + 10] = b'x';
    assert!(matches!(
        rewrite(&bad, &parse_any("slack").unwrap(), &Options::default()),
        Some((Rewrite::Kept { .. }, None))
    ));
}
