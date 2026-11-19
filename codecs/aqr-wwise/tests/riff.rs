//! The walk of a medium's RIFF chunks: the pad byte after a chunk of odd size is taken only
//! where what follows reads as a tag after it and not before it.

use aqr_wwise::wem;

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8], pad: bool) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if pad && body.len() % 2 == 1 {
        out.push(0);
    }
}

/// A medium whose `fmt ` (Vorbis, no `vorb` chunk: it is reached only if the walk finds it)
/// comes after an `odd` chunk of 5 bytes, padded or not.
fn medium(pad: bool) -> Vec<u8> {
    let mut d = b"RIFF\0\0\0\0WAVE".to_vec();
    chunk(&mut d, b"junk", b"12345", pad);
    let mut fmt = vec![0xFF, 0xFF, 2, 0];
    fmt.extend_from_slice(&44100u32.to_le_bytes());
    fmt.extend_from_slice(&[0; 14]);
    chunk(&mut d, b"fmt ", &fmt, true);
    chunk(&mut d, b"data", &[0; 8], true);
    let n = d.len() as u32 - 8;
    d[4..8].copy_from_slice(&n.to_le_bytes());
    d
}

#[test]
fn odd_chunk_padded_or_not() {
    // reaching the `fmt ` chunk, the walk goes on to want a `vorb`
    assert_eq!(wem::parse(&medium(true)).err(), Some("no vorb"));
    assert_eq!(wem::parse(&medium(false)).err(), Some("no vorb"));
}

#[test]
fn odd_chunk_truncated_and_junk() {
    let d = medium(true);
    for n in 0..d.len() {
        let _ = wem::parse(&d[..n]);
    }
    // an odd chunk at the very end, then nothing, or a lone pad
    let mut e = b"RIFF\0\0\0\0WAVE".to_vec();
    chunk(&mut e, b"junk", b"1", false);
    assert_eq!(wem::parse(&e).err(), Some("no fmt"));
    e.push(0);
    assert_eq!(wem::parse(&e).err(), Some("no fmt"));
}
