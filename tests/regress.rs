//! Round trips of the files in tests/data (small synthetic FLAC, WAV and AIFF files, see
//! tools/gen_testdata.sh) through the coded path and the container, also with junk around,
//! cut short, and as headerless PCM.

use flac_recons::container::{compress_with, roundtrip};
use flac_recons::pcm::Format;
use flac_recons::{compress, decompress};

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

fn check(name: &str, d: &[u8], raw: Option<Format>) {
    assert!(roundtrip(d, raw), "{name}: coded path");
    assert_eq!(decompress(&compress_with(d, raw)).unwrap(), d, "{name}: container");
}

#[test]
fn data_files() {
    for (name, d) in files() {
        check(&name, &d, None);
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
        check(&name, &j, None);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            check(&name, &d[..d.len() - cut.min(d.len())], None);
            check(&name, &d[cut.min(d.len())..], None);
        }
        // Two files concatenated.
        let mut two = d.clone();
        two.extend_from_slice(&files()[0].1);
        check(&name, &two, None);
    }
}

#[test]
fn raw_pcm() {
    for (name, d) in files() {
        for f in ["s16le:2", "s24be:2", "u8:1", "s32le:6", "s16be:3"] {
            let fmt = Format::parse(f).unwrap();
            check(&format!("{name} as {f}"), &d, Some(fmt));
            check(&format!("{name} as {f}, cut"), &d[..d.len() * 2 / 3 + 1], Some(fmt));
        }
    }
}

#[test]
fn corrupt_containers() {
    for (_, d) in files() {
        let c = compress(&d);
        for i in (5..c.len()).step_by(97) {
            let mut m = c.clone();
            m[i] ^= 0x5a;
            // Must not panic; mostly an error or a checksum mismatch.
            let _ = decompress(&m);
        }
        for cut in [6, 20, c.len() / 2] {
            let _ = decompress(&c[..c.len() - cut.min(c.len())]);
        }
    }
}
