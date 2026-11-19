//! Streams damaged, empty, over their bounds and followed by other bytes, through `extent`,
//! `decode`, the split and the formula: a stream is refused or rebuilt exactly, with no panic,
//! no hang and no allocation its bytes cannot hold.

use std::time::Duration;

use aqr_bvx::{bvx, formula, recons};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 28);

const LIMIT: usize = 1 << 24;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.lzfse", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// What every layer makes of `s`: refused, or consistent with the others.
fn check(s: &[u8]) {
    let ext = bvx::extent(s);
    if let Some((e, _)) = ext {
        assert!(e <= s.len() && e >= 4);
    }
    if let Ok(st) = bvx::decode(s, LIMIT) {
        assert!(st.used <= s.len() && st.data.len() <= LIMIT);
        // (what is decoded was extent's too, with the same bytes and data)
        assert_eq!(ext, Some((st.used, st.data.len() as u64)));
        let mut out = Vec::new();
        if bvx::write(&st.data, &st.blocks, &mut out).is_ok() {
            assert!(out == s[..st.used], "written otherwise");
        }
        if let Ok(sp) = recons::split(s, LIMIT) {
            assert!(sp.data == st.data && sp.used == st.used);
            let mut out = Vec::new();
            recons::join_onto(&sp.data, &sp.record, &sp.counts, &mut out).unwrap();
            assert!(out == s[..sp.used], "rebuilt otherwise");
        }
    }
    if let Some(f) = formula::encode(s, 1) {
        assert!(formula::starts(s));
        assert!(f.streams.iter().any(|x| x.class == "raw"));
    }
}

#[test]
fn damaged_streams() {
    for name in ["small", "stored", "empty"] {
        let s = fixture(name);
        aqr_testkit::damaged(&s, 400, s.len().min(80), Duration::from_secs(20), check);
    }
}

#[test]
fn damaged_big_streams() {
    for name in ["text", "icon"] {
        let s = fixture(name);
        aqr_testkit::damaged(&s, 40, 80, Duration::from_secs(60), check);
    }
}

#[test]
fn empty_data() {
    let s = fixture("empty");
    assert_eq!(bvx::extent(&s), Some((s.len(), 0)));
    assert!(formula::encode(&s, 1).is_none(), "no data: nothing to rebuild");
    // only an end block is no stream (nothing before it), and a stored block of nothing is data of none
    assert_eq!(bvx::extent(b"bvx$"), None);
    assert!(formula::encode(b"bvx$", 1).is_none());
    let st = bvx::decode(&s, 0).unwrap();
    assert!(st.data.is_empty());
    assert!(bvx::decode(b"bvx$", 0).unwrap().data.is_empty());
    assert!(bvx::decode(b"", 0).is_err() && bvx::extent(b"").is_none());
    assert!(!formula::starts(b"bvx") && !formula::starts(b""));
}

#[test]
fn over_the_bounds() {
    // blocks that say more data than they hold, as stored (past the bytes) or as LZVN (a
    // payload that cannot make them), where the limit is
    let mut s = b"bvx-".to_vec();
    s.extend(u32::MAX.to_le_bytes());
    s.extend(b"bvx$");
    assert!(bvx::extent(&s).is_none() && bvx::decode(&s, usize::MAX).is_err());
    assert!(formula::encode(&s, 1).is_none());

    let mut s = b"bvxn".to_vec();
    s.extend(u32::MAX.to_le_bytes());
    s.extend(1u32.to_le_bytes());
    s.extend([0x06]);
    s.extend(b"bvx$");
    // (the headers hold; the data is more than MAX_DATA's 1 GiB, so no formula, and no decoding
    // of it either: no allocation of the size)
    assert_eq!(bvx::extent(&s), Some((s.len(), u32::MAX as u64)));
    aqr_testkit::case("lzvn of 4 GiB");
    assert!(formula::encode(&s, 1).is_none());
    assert!(bvx::decode(&s, LIMIT).is_err());
    assert!(recons::split(&s, LIMIT).is_err());
    assert!(aqr_testkit::most() < LIMIT * 2, "{} bytes asked for", aqr_testkit::most());
    // the same of blocks of data that sum past a u64... or a usize
    let mut s = Vec::new();
    for _ in 0..3 {
        s.extend(b"bvx-");
        s.extend(0u32.to_le_bytes());
    }
    s.extend(b"bvx$");
    assert_eq!(bvx::extent(&s), Some((s.len(), 0)));
}

#[test]
fn bytes_after_the_end() {
    let s = fixture("small");
    let st = bvx::decode(&s, LIMIT).unwrap();
    for tail in [&b"x"[..], b"bvx", b"bvx$", b"bvx-\0\0\0\0", &[0u8; 1000]] {
        let t = [&s[..], tail].concat();
        assert_eq!(bvx::extent(&t).map(|e| e.0), Some(s.len()), "{tail:?}");
        let d = bvx::decode(&t, LIMIT).unwrap();
        assert_eq!((d.used, &d.data), (s.len(), &st.data));
        let f = formula::encode(&t, 1).unwrap();
        // the bytes after it are a stream of their own, taken as they are
        let rest = f.streams.iter().find(|x| x.class == "bvx").unwrap();
        assert_eq!(&rest.data[..], tail);
        let sp = recons::split(&t, LIMIT).unwrap();
        assert_eq!(sp.used, s.len());
    }
    // a stream cut before its end block is none
    assert!(bvx::extent(&s[..s.len() - 4]).is_none());
    assert!(formula::encode(&s[..s.len() - 4], 1).is_none());
}

/// The LZVN opcodes, one byte of each value at the start of a block, and the LZFSE headers'
/// fields at their extremes: refused or consistent.
#[test]
fn opcodes_and_fields() {
    for op in 0..=255u8 {
        for tail in [&[][..], &[1], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]] {
            let mut payload = vec![op];
            payload.extend(tail);
            payload.extend([0x06; 8]);
            let mut s = b"bvxn".to_vec();
            s.extend(((payload.len() as u32) * 3).to_le_bytes());
            s.extend((payload.len() as u32).to_le_bytes());
            s.extend(&payload);
            s.extend(b"bvx$");
            aqr_testkit::case(&format!("op {op:#x}"));
            check(&s);
        }
    }
    // the version-2 block's header with each field at 0 and at its largest
    let t = fixture("text");
    assert_eq!(&t[..4], b"bvx2");
    let v2 = 0;
    for at in 4..32 {
        for x in [0x00, 0xff, 0x80, 0x7f] {
            let mut s = t.clone();
            s[v2 + at] = x;
            aqr_testkit::case(&format!("v2 field byte {at} = {x:#x}"));
            check(&s);
        }
    }
}
