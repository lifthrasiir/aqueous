//! Files whose digests others hold, kept as they are (`digests`).

use super::digests::{ALGORITHMS, digests, forms};
use super::*;
use crate::{Input, compress_tree};

#[test]
fn known_forms() {
    let d = digests(b"abc");
    assert_eq!(ALGORITHMS[2], "SHA-256");
    let [raw, lower, upper, b64, url] = forms(&d[2]);
    assert_eq!(raw.len(), 32);
    assert_eq!(lower, b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(upper, lower.to_ascii_uppercase());
    // (SRI's form of it, padded: `sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=`)
    assert_eq!(b64, b"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0");
    assert_eq!(url, b"ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0");
    assert_eq!(digests(b"abc")[0], forms(&digests(b"abc")[0])[0]);
    let md5: Vec<u8> = forms(&d[0])[1].clone();
    assert_eq!(md5, b"900150983cd24fb0d6963f7d28e17f72");
}

/// A gzip file of libdeflate's, which `coding` rewrites at level 8, archived with `other`
/// beside it: what became of it, and what it was given back as.
fn with(other: Option<(&str, Vec<u8>)>) -> (Rewrite, bool) {
    let words = ["alpha ", "beta ", "gamma ", "delta "];
    let data: Vec<u8> = (0..400_000u32)
        .flat_map(|i| words[(i * 7 % 4) as usize].bytes().chain(format!("{} ", i % 97).into_bytes()))
        .collect();
    let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    gz.extend("libdeflate:level=9".parse::<flate_zoo::Encoder>().unwrap().compress(&data).unwrap());
    let mut crc = flate2::Crc::new();
    crc.update(&data);
    gz.extend(crc.sum().to_le_bytes());
    gz.extend((data.len() as u32).to_le_bytes());
    let mut names = vec![b"a.txt.gz".to_vec()];
    let mut datas = vec![gz.clone()];
    if let Some((n, d)) = other {
        names.push(n.as_bytes().to_vec());
        datas.push(d);
    }
    let tree = aqr_archive::Tree {
        entries: (0..names.len())
            .map(|c| aqr_archive::Entry {
                parent: None,
                name: names[c].clone(),
                node: aqr_archive::EntryNode::File(c),
                extra: Default::default(),
            })
            .collect(),
        ..Default::default()
    };
    let inputs: Vec<Input> = (0..names.len()).map(|c| Input { name: &names[c], data: &datas[c] }).collect();
    let opt = Options { threads: 2, lossy: Some(Lossy::parse("coding").unwrap()), ..super::tests::high() };
    let a = compress_tree(&tree, &inputs, &opt);
    let aqr_archive::Value::Tree(_, got) = aqr_decompress::decompress_value(&a.archive, 1).unwrap() else {
        panic!("a tree")
    };
    (a.report.files[0].lossy.clone().unwrap(), got.contains(&gz))
}

#[test]
fn digests_held_kept() {
    let gz_digests = |algorithm: usize| {
        let (w, _) = with(None);
        assert!(matches!(w, Rewrite::Made { .. }), "{w:?}");
        // (the file as it was: of `with`'s making, again)
        let words = ["alpha ", "beta ", "gamma ", "delta "];
        let data: Vec<u8> = (0..400_000u32)
            .flat_map(|i| words[(i * 7 % 4) as usize].bytes().chain(format!("{} ", i % 97).into_bytes()))
            .collect();
        let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        gz.extend("libdeflate:level=9".parse::<flate_zoo::Encoder>().unwrap().compress(&data).unwrap());
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        gz.extend(crc.sum().to_le_bytes());
        gz.extend((data.len() as u32).to_le_bytes());
        forms(&digests(&gz)[algorithm])
    };
    let [_, sha256_hex, ..] = gz_digests(2);
    let sums = [&sha256_hex[..], b"  a.txt.gz\n"].concat();
    let (w, as_it_was) = with(Some(("SHA256SUMS", sums)));
    assert!(matches!(&w, Rewrite::Kept { reason, .. } if reason == "its SHA-256 is in SHA256SUMS"), "{w:?}");
    assert!(as_it_was);
    // SRI in a lockfile: sha512-, base64 with padding
    let [.., sha512_b64, _] = gz_digests(4);
    let lock = [&b"{\"integrity\": \"sha512-"[..], &sha512_b64, b"==\"}"].concat();
    let (w, as_it_was) = with(Some(("package-lock.json", lock)));
    assert!(matches!(&w, Rewrite::Kept { reason, .. } if reason.contains("SHA-512 is in package-lock.json")), "{w:?}");
    assert!(as_it_was);
    // (an unrelated digest: rewritten)
    let (w, as_it_was) =
        with(Some(("SHA256SUMS", b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  x\n".to_vec())));
    assert!(matches!(w, Rewrite::Made { .. }), "{w:?}");
    assert!(!as_it_was);
}
