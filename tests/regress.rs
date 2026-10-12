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
        for i in (flac_recons::container::HEADER_LEN..c.len()).step_by(97) {
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

/// Container header: the magic of the input's kind, then the version (0 stored, 1 coded).
#[test]
fn header() {
    let hdr = |c: &[u8]| (c[..6].to_vec(), u16::from_le_bytes([c[6], c[7]]));
    let (flac, wav, pcm) = (b"\x69\x04\x7e\x7f\x56\x9c", b"\x69\x04\x7e\xc1\xab\xf7", b"\x69\x04\x7e\xa5\xc9\xbf");
    for (name, d) in files() {
        let want = if name.ends_with(".flac") { flac } else { wav };
        assert_eq!(hdr(&compress(&d)), (want.to_vec(), 1), "{name}");
        // WAV / AIFF as raw samples of their own format (header bytes and all).
        if let Some((fmt, _, _)) = flac_recons::pcm::detect(&d) {
            assert_eq!(hdr(&compress_with(&d, Some(fmt))), (pcm.to_vec(), 1), "{name} as raw");
        }
    }
    // Stored: unrecognised input, and inputs of a known kind that do not get smaller.
    let junk = b"not audio at all".to_vec();
    let c = compress(&junk);
    assert_eq!(hdr(&c), (pcm.to_vec(), 0));
    assert_eq!(&c[8..], &junk[..]);
    assert_eq!(decompress(&c).unwrap(), junk);
    let tiny_flac = b"fLaC\x80\x00\x00\x22".iter().copied().chain([0u8; 34]).collect::<Vec<u8>>();
    assert_eq!(hdr(&compress(&tiny_flac)), (flac.to_vec(), 0));
    let tiny_wav = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x44\xac\x00\x00\x88\x58\x01\x00\x02\x00\x10\x00data\x00\x00\x00\x00";
    assert_eq!(hdr(&compress(tiny_wav)), (wav.to_vec(), 0));
    assert_eq!(hdr(&compress_with(b"ab", Format::parse("s16le:1"))), (pcm.to_vec(), 0));
    // Other versions and the old header are rejected.
    let mut v2 = compress(&junk);
    v2[6] = 2;
    assert!(decompress(&v2).is_err());
    assert!(decompress(b"FLR\x01\x00abc").is_err());
}
