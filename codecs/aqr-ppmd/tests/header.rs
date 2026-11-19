//! The header of a PPMd var. J file: what is refused, and the edges of what is taken.

use aqr_ppmd::{Header, SIGNATURE, formula, header, starts};

/// A file: the header of `order` (2-16), `mb` (1-256), variant `var` (0 for A), `restore` and a
/// name of `name` bytes, then `data` bytes.
fn file(order: u16, mb: u16, var: u16, restore: u16, name: usize, data: usize) -> Vec<u8> {
    let mut d = SIGNATURE.to_vec();
    d.extend([0x20, 0, 0, 0]);
    d.extend(((order - 1) | (mb - 1) << 4 | var << 12).to_le_bytes());
    d.extend((name as u16 | restore << 14).to_le_bytes());
    d.extend([0; 4]);
    d.extend(vec![b'n'; name]);
    d.extend(vec![7; data]);
    d
}

#[test]
fn what_is_taken() {
    let h = |d: &[u8]| header(d);
    for order in [2, 3, 8, 16] {
        for mb in [1, 2, 255, 256] {
            for restore in 0..3 {
                let d = file(order, mb, b'J' as u16 - b'A' as u16, restore, 6, 1);
                let want = Header { len: 22, order: order as u8, memory_mb: mb, variant: 'J', restore: restore as u8 };
                assert_eq!(h(&d), Some(want), "{order} {mb} {restore}");
                assert!(starts(&d));
                assert!(formula::encode(&d, 1).is_some());
            }
        }
    }
    // the longest name (14 bits), one byte of data after it
    let d = file(10, 16, 9, 1, 0x3fff, 1);
    assert_eq!(h(&d).unwrap().len, 16 + 0x3fff);
    // an empty name
    assert_eq!(h(&file(10, 16, 9, 1, 0, 1)).unwrap().len, 16);
}

#[test]
fn what_is_refused() {
    let j = b'J' as u16 - b'A' as u16;
    // the restore method 3, the order 1 (the field 0), variants other than J
    assert!(header(&file(10, 16, j, 3, 4, 5)).is_none());
    let mut d = file(10, 16, j, 1, 4, 5);
    d[8] &= 0xf0;
    assert!(header(&d).is_none(), "order 1");
    for var in (0..16).filter(|&v| v != j) {
        assert!(header(&file(10, 16, var, 1, 4, 5)).is_none(), "variant {var}");
    }
    // the name reaching or passing the end (the data is at least a byte), and the header cut
    let d = file(10, 16, j, 1, 4, 0);
    assert!(header(&d).is_none() && !starts(&d) && formula::encode(&d, 1).is_none());
    let d = file(10, 16, j, 1, 100, 0);
    assert!(header(&d[..d.len() - 1]).is_none());
    let mut d = file(10, 16, j, 1, 4, 3);
    d[10..12].copy_from_slice(&0x3fffu16.to_le_bytes());
    assert!(header(&d).is_none(), "name past the buffer");
    // short inputs, every prefix of a file
    let d = file(10, 16, j, 1, 4, 3);
    for n in 0..d.len() {
        let h = header(&d[..n]);
        assert_eq!(h.is_some(), n > 20, "prefix {n}");
    }
    // the signature
    for i in 0..4 {
        let mut d = file(10, 16, j, 1, 4, 3);
        d[i] ^= 1;
        assert!(header(&d).is_none());
    }
    assert!(header(b"").is_none());
}

#[test]
fn damaged_headers() {
    let d = file(10, 16, 9, 1, 12, 40);
    aqr_testkit::damaged(&d, 64, d.len(), std::time::Duration::from_secs(5), |v| {
        if let Some(h) = header(v) {
            assert!(h.len < v.len() && (2..=16).contains(&h.order) && h.restore <= 2);
            let f = formula::encode(v, 1).unwrap();
            assert_eq!(f.streams[0].data.len(), h.len);
        }
    });
}
