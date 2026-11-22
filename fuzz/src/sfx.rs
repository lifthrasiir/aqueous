//! `sfx`'s body ([`sfx`]): what a self-extracting program reads of what follows its image, as
//! its stub reads it (aqr-sfx's portable part): the bundle of its blobs (zstd or LZMA2 frames,
//! the x86 filter undone), and the record of its settings at the start and at the end of what is
//! there. A record read is the same written again and read back.

use aqr_sfx::config::Config;

/// What of `d` reads (for `replay`): the blobs of its bundle, and the records of settings found.
pub fn told(d: &[u8]) -> String {
    let blobs = aqr_sfx::bundle::unpack(d).map(|b| b.len());
    let at = Config::at(d).map(|r| r.is_ok());
    let before = Config::before(d, d.len()).map(|r| r.is_ok());
    format!("bundle {blobs:?}, a record at the start {at:?}, at the end {before:?}")
}

pub fn sfx(d: &[u8]) {
    let _ = aqr_sfx::bundle::len(d);
    let _ = aqr_sfx::bundle::unpack(d);
    let found = [Config::at(d), Config::before(d, d.len())].into_iter().flatten().filter_map(Result::ok);
    for c in found.map(|(c, _)| c).chain(Config::from_cbor(d).ok()) {
        match Config::from_cbor(&c.to_cbor()) {
            Ok(back) => assert!(back == c, "settings read back otherwise\n{back:?}\n{c:?}"),
            Err(e) => panic!("settings written again not read: {e}\n{c:?}"),
        }
    }
}

/// The seeds of `sfx`: bundles of a few blobs, records of settings of several `sfx.toml`s (alone,
/// and after a bundle, as the program has them).
pub fn seeds() -> Vec<(String, Vec<u8>)> {
    use aqr_sfx::config::{Kind, compile};
    let code = |n: usize, seed: u8| -> Vec<u8> {
        // (bytes of x86's calls and jumps now and then, for the filter)
        (0..n).map(|i| if i % 13 == 0 { 0xe8 } else { (i as u8).wrapping_mul(seed) ^ (i >> 5) as u8 }).collect()
    };
    // (blobs of an image of one section of code, as aqr-ld digests a library; relocations of a
    // few places)
    let blob = |name: &str, n: usize, seed: u8, apply: u32| -> Vec<u8> {
        let image = code(n, seed);
        aqr_blob::Blob {
            apply_rva: apply,
            info_rva: 0,
            pdata_rva: 0,
            pdata_count: 0,
            import_dir_rva: 0,
            first: 0x1000,
            image_base: 0x1_8000_0000,
            name: name.to_string(),
            binds: Vec::new(),
            relocs: (0..8).map(|k| 0x1000 + 64 * k).collect(),
            sections: vec![aqr_blob::Section {
                rva: 0x1000,
                len: n as u32,
                flags: aqr_blob::SCN_EXECUTE | aqr_blob::SCN_READ,
            }],
            image,
        }
        .to_bytes()
    };
    let hash = "0".repeat(64);
    let blobs = vec![
        (format!("{hash}/1"), blob("", 20_000, 7, 0x1010)),
        ("lib:shared".to_string(), blob("shared.dll", 9_000, 11, 0)),
        ("aqr-interp".to_string(), blob("", 4_000, 3, 0x1020)),
    ];
    let mut out = Vec::new();
    let bundle = aqr_sfx::bundle::pack(&blobs).expect("a bundle");
    out.push(("bundle".to_string(), bundle.clone()));
    if let Ok(xz) = aqr_sfx::bundle::pack_xz(&blobs) {
        out.push(("bundle-xz".to_string(), xz));
    }
    let tomls = [
        "title = { en = \"Setup\", ko = \"설치\" }\n[ui]\nlicense = \"license.txt\"\nprompt = \"Go?\"\n",
        "mode = \"extract\"\n[extract]\ndest = \"{cwd}/out\"\noverwrite = \"newer\"\n",
        "mode = \"run\"\n[run]\nprogram = [\"bin/app.exe\", \"--flag\", \"x y\"]\ncwd = \"bin\"\nwindow = \"hidden\"\nwait = true\ncleanup = true\nexit-code = true\nenv = { A = \"1\", PATH_EXTRA = \"bin\" }\n",
        "[ui]\nbanner = \"banner.png\"\nprompt = { en = \"Go?\", ko = \"할까요?\" }\n",
    ];
    let files: std::collections::HashMap<String, Vec<u8>> = [
        ("license.txt", &b"MIT"[..]),
        ("license.ko.txt", "MIT, 한국어".as_bytes()),
        ("banner.png", b"\x89PNG\r\n\x1a\n..."),
    ]
    .into_iter()
    .map(|(n, d)| (n.to_string(), d.to_vec()))
    .collect();
    for (k, toml) in tomls.into_iter().enumerate() {
        let mut fs = files.clone();
        fs.insert("sfx.toml".into(), toml.as_bytes().to_vec());
        let kind = |p: &str| match p {
            "bin" => Some(Kind::Dir),
            "bin/app.exe" => Some(Kind::File),
            _ => None,
        };
        match compile(&fs, &kind) {
            Ok(c) => {
                let r = c.config.record();
                out.push((format!("record-{k}"), r.clone()));
                out.push((format!("bundle-record-{k}"), [&bundle[..], &r].concat()));
            }
            Err(e) => eprintln!("sfx.toml {k}: {e}"),
        }
    }
    out
}
