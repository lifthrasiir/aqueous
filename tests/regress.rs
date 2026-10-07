//! Round trips of the files in tests/data (small LAME encodings of a synthetic signal),
//! through the coded path and the container, also with junk around and cut short.

use mp3_recons::{codec, compress, decompress};

fn files() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

fn roundtrip(name: &str, d: &[u8]) {
    let c = codec::encode(d);
    assert_eq!(codec::decode(&c).unwrap(), d, "{name}: coded path");
    assert_eq!(decompress(&compress(d)).unwrap(), d, "{name}: container");
}

#[test]
fn data_files() {
    for (name, d) in files() {
        roundtrip(&name, &d);
        // These should all be coded, and smaller than the input.
        let c = compress(&d);
        assert!(c.len() < d.len(), "{name}: {} >= {}", c.len(), d.len());
    }
}

#[test]
fn junk_and_cuts() {
    for (name, d) in files() {
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&d);
        j.extend_from_slice(b"TAG and after");
        roundtrip(&name, &j);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            roundtrip(&name, &d[..d.len() - cut.min(d.len())]);
            roundtrip(&name, &d[cut.min(d.len())..]);
        }
        // Two streams concatenated (sample rate and mode may change).
        let mut two = d.clone();
        two.extend_from_slice(&files()[0].1);
        roundtrip(&name, &two);
    }
}

#[test]
fn corrupt_containers() {
    for (name, d) in files().into_iter().take(3) {
        let c = compress(&d);
        for i in (5..c.len()).step_by(97) {
            let mut m = c.clone();
            m[i] ^= 0x5a;
            // Must not panic; mostly an error or a checksum mismatch.
            let _ = decompress(&m);
            let _ = &name;
        }
        for cut in [6, 20, c.len() / 2] {
            let _ = decompress(&c[..c.len() - cut]);
        }
    }
}
