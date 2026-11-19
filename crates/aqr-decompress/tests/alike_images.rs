//! Native and WebAssembly decoding alike on damaged inputs (fail_alike), for the modules
//! of ZIP, git, flate-set, GIF, Clip Studio and JPEG (lossy and lossless): every stream of a small
//! application damaged in turn decodes to the same bytes or fails in both.

#![cfg(feature = "wasm")]

#[path = "../../../codecs/aqr-clip/tests/common/mod.rs"]
mod clip_files;

use std::time::Duration;

use aqr_formula::{Formula, Node};

/// As the helper `common::fail_alike` (every stream of `f` damaged in turn, decoded natively and as
/// Wasm), but a failure in both counts as alike whatever each says (the messages differ: a
/// trap's text against the module's own).
fn fail_alike(name: &str, f: &Formula, per: usize, head: usize, limit: Duration) {
    use aqr_archive::{Encoder, Level};
    use aqr_decompress::{Native, Wasm, wasm_of};
    let enc = Encoder::new(Level::Zstd(1)).with_modules(wasm_of);
    let mut bad = Vec::new();
    for k in 0..f.streams.len() {
        let d = f.streams[k].data.to_vec();
        for x in aqr_testkit::damages(d.len(), per, head) {
            let mut g = f.clone();
            g.streams[k].data = x.apply(&d).into();
            let Ok(c) = enc.encode(&g, None) else { continue };
            let what = format!("{name}: stream {k} ({}), {x}", f.streams[k].class);
            match aqr_testkit::within(limit, || {
                (aqr_archive::decode(&c, &Native::default()), aqr_archive::decode(&c, &Wasm))
            }) {
                Err(e) => bad.push(format!("{what}: {e}")),
                Ok((Ok(n), Ok(w))) if n != w => bad.push(format!("{what}: other bytes natively and as Wasm")),
                Ok((Ok(_), Err(e))) => bad.push(format!("{what}: decoded natively, {e} as Wasm")),
                Ok((Err(e), Ok(_))) => bad.push(format!("{what}: {e} natively, decoded as Wasm")),
                Ok(_) => {}
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} cases not alike:
{}",
        bad.len(),
        bad.join(
            "
"
        )
    );
}

const LIMIT: Duration = Duration::from_secs(30);

fn text(n: usize, salt: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        t.extend_from_slice(
            format!("line {i}: the quick brown fox jumps over {} lazy dogs\n", (i * 7 + salt) % 13).as_bytes(),
        );
    }
    t.truncate(n);
    t
}

/// A raw DEFLATE stream of `d`, as zlib's level 6 makes it.
fn deflate(d: &[u8]) -> Vec<u8> {
    aqr_flate::zlib::compress(d, aqr_flate::zlib::Params { level: 6, ..Default::default() }).unwrap()
}

fn le(o: &mut Vec<u8>, v: u64, n: usize) {
    o.extend_from_slice(&v.to_le_bytes()[..n]);
}

/// A ZIP archive of entries (name, data, body, method): local headers, central directory, end.
fn zip(entries: &[(&str, &[u8], Vec<u8>, u16)]) -> Vec<u8> {
    let mut o = Vec::new();
    let mut central = Vec::new();
    for (name, data, body, method) in entries {
        let at = o.len() as u64;
        let crc = aqr_common::crc32::hash(data) as u64;
        for (out, local) in [(&mut o, true), (&mut central, false)] {
            out.extend_from_slice(if local { b"PK\x03\x04" } else { b"PK\x01\x02" });
            if !local {
                le(out, 20, 2);
            }
            le(out, 20, 2);
            le(out, 0, 2);
            le(out, *method as u64, 2);
            le(out, 0, 4);
            le(out, crc, 4);
            le(out, body.len() as u64, 4);
            le(out, data.len() as u64, 4);
            le(out, name.len() as u64, 2);
            le(out, 0, 2);
            if !local {
                le(out, 0, 4);
                le(out, 0, 4);
                le(out, 0, 2);
                le(out, at, 4);
            }
            out.extend_from_slice(name.as_bytes());
            if local {
                out.extend_from_slice(body);
            }
        }
    }
    let n = entries.len() as u64;
    let start = o.len() as u64;
    o.extend_from_slice(&central);
    o.extend_from_slice(b"PK\x05\x06");
    le(&mut o, 0, 4);
    le(&mut o, n, 2);
    le(&mut o, n, 2);
    le(&mut o, central.len() as u64, 4);
    le(&mut o, start, 4);
    le(&mut o, 0, 2);
    o
}

#[test]
fn zip_recons_alike() {
    let a = text(1500, 1);
    let b = text(900, 2);
    let (da, db) = (deflate(&a), deflate(&b));
    // a body stored twice (Made::Same), another deflated and a stored one
    let file = zip(&[
        ("a.txt", &a, da.clone(), 8),
        ("b.txt", &b, db, 8),
        ("a2.txt", &a, da, 8),
        ("c.bin", &b[..300], b[..300].to_vec(), 0),
    ]);
    let f = aqr_zip::formula::encode(&file, 1).expect("zip formula");
    fail_alike("zip-recons", &f, 6, 3, LIMIT);
}

/// A pack of blob entries of the given data.
fn pack(datas: &[Vec<u8>]) -> Vec<u8> {
    use aqr_git::pack;
    let mut p = b"PACK\0\0\0\x02".to_vec();
    p.extend_from_slice(&(datas.len() as u32).to_be_bytes());
    for d in datas {
        pack::put_header(pack::BLOB, d.len() as u64, &mut p);
        p.extend_from_slice(&[0x78, 0x9c]);
        p.extend(deflate(d));
        p.extend_from_slice(&aqr_common::adler32::hash(d).to_be_bytes());
    }
    let sum = pack::sha1(&p);
    p.extend_from_slice(&sum);
    p
}

fn git_pack() -> (Vec<u8>, Formula) {
    let datas = vec![
        text(700, 1),
        text(300, 4),
        (0..400u32).map(|i| (i.wrapping_mul(2_654_435_761u32) >> 24) as u8).collect(),
        text(50, 3),
    ];
    let p = pack(&datas);
    let f = aqr_git::formula::encode(&p, 1).expect("git formula");
    (p, f)
}

#[test]
fn git_pack_recons_alike() {
    let (_, f) = git_pack();
    fail_alike("git-pack-recons", &f, 6, 3, LIMIT);
}

#[test]
fn git_pack_index_recons_alike() {
    let (p, f) = git_pack();
    let n = 4;
    let ids: Vec<u8> = (0..n * 20u32).map(|i| (i.wrapping_mul(40_503) >> 5) as u8).collect();
    let (idx_len, rev_len) = (1072 + 28 * n as usize, 12 + 4 * n as usize + 40);
    assert!(aqr_git::formula::is_pack(&f, p.len()));
    for rev in [None, Some(rev_len)] {
        let g = aqr_git::formula::with_index(f.clone(), ids.clone(), idx_len, rev);
        fail_alike(&format!("git-pack-index-recons rev={}", rev.is_some()), &g, 6, 3, LIMIT);
    }
}

#[test]
fn flate_set_alike() {
    use aqr_flate::formula::apply;
    use aqr_flate::module::{Derived, index, index_before};
    let datas = [text(1200, 1), text(500, 2), Vec::new(), text(2500, 3)];
    let parts: Vec<(Vec<u8>, Vec<u8>, usize)> = datas
        .iter()
        .map(|d| {
            let raw = deflate(d);
            let (data, record) = aqr_flate::deconstruct(&raw).unwrap();
            (data, record, raw.len())
        })
        .collect();
    let data: Vec<u8> = parts.iter().flat_map(|p| p.0.clone()).collect();
    let records: Vec<u8> = parts.iter().flat_map(|p| p.1.clone()).collect();
    let lens: Vec<_> = parts.iter().map(|p| (p.0.len(), p.1.len(), p.2)).collect();
    let crcs: Vec<_> = datas.iter().zip(&lens).map(|(d, l)| (l.0, l.1, l.2, aqr_common::crc32::hash(d))).collect();
    for derived in [Derived::None, Derived::Zip, Derived::Gzip, Derived::ZipBefore, Derived::Zlib] {
        let idx = if derived.before() { index_before(&crcs) } else { index(&lens) };
        let len = parts.iter().map(|p| (p.2 + derived.bytes()) as u64).sum();
        let mut f = Formula::default();
        let (d, r, i) =
            (f.push("flate/data", data.clone()), f.push("flate/record", records.clone()), f.push("flate/index", idx));
        let a = apply(aqr_flate::DEFAULT_PROFILE, d, r, i, len, derived).unwrap();
        f.root = Node::Apply(Box::new(a));
        fail_alike(&format!("flate-set {derived:?}"), &f, 5, 3, LIMIT);
    }
}

#[test]
fn gif_recons_alike() {
    let d = include_bytes!("../../../codecs/aqr-gif/tests/data/pil4-photo.gif");
    let f = aqr_gif::formula::encode(d, 1).expect("gif formula");
    fail_alike("gif-recons", &f, 6, 4, LIMIT);
}

#[test]
fn clip_recons_alike() {
    let file = clip_files::sample(0);
    let f = aqr_clip::formula::encode(&file, 1).expect("clip formula");
    fail_alike("clip-recons", &f, 4, 3, LIMIT);
}

#[test]
fn jpeg_recons_alike() {
    let d = include_bytes!("../../../codecs/aqr-jpeg/tests/data/gradient.jpg");
    let f = aqr_jpeg::formula::encode(d, 1).expect("jpeg formula");
    fail_alike("jpeg-recons", &f, 6, 4, LIMIT);
}

#[test]
fn jpeg_lossless_recons_alike() {
    use aqr_jpeg::lossless::synthetic;
    for (name, d) in [
        ("gray 12", synthetic(12, 24, 12, &[(1, 1)], 1, 0, 0, 3)),
        ("two comps 8, restarts", synthetic(8, 16, 8, &[(1, 1), (1, 1)], 4, 0, 16, 5)),
    ] {
        let f = aqr_jpeg::lformula::encode(&d, 1).unwrap_or_else(|| panic!("{name}: formula"));
        fail_alike(&format!("jpeg-lossless-recons {name}"), &f, 6, 4, LIMIT);
    }
}
